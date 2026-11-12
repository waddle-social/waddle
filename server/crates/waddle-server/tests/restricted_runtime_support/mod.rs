//! Fixtures shared by the restricted-runtime test binaries: a database with
//! separate owner and runtime roles, the owner phase and attestor on it, the
//! operator handoff, a recording relay in front of PostgreSQL, and the
//! stanza helpers the serving tests use.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use minidom::Element;
use sqlx::postgres::{PgConnectOptions, PgPool};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{AttestedLineage, DeploymentUuid, LineageAction, PgSchemaName};
use waddle_server::db::physical_attestation::{install, PgRoleName, PhysicalAttestationConfig};
use waddle_server::db::runtime_startup::{compose_handoff, HandoffInputs, OperatorRecord};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::server::runtime_owner::{
    prepare_restricted_runtime_owner, RestrictedRuntimeOwnerError, RestrictedRuntimeOwnerPlan,
};
use waddle_ws_test_support::iq::send_iq;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::stream_management::SM_NS;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::ping::Ping;

pub const RELEASE_GIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
pub const SESSION_KEY: &str = "integration-test-session-key-32-bytes-long";
pub const SCHEMA: &str = "guarded";
pub const TRUSTED_SCHEMA: &str = "attestor";
pub const RESTRICTED: &str = "NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS";

/// What crossed the runtime login's PostgreSQL connections: every error the
/// server returned and every statement that would change the catalog.
#[derive(Default)]
pub struct Wire {
    pub errors: Vec<(String, String)>,
    pub catalog_statements: Vec<String>,
}

/// Incremental PostgreSQL wire-protocol framing for one direction of one
/// connection. The frontend starts with untyped startup packets; an SSL or
/// GSS negotiation request is answered by one unframed byte.
pub struct Frames {
    pub buffer: Vec<u8>,
    pub startup: bool,
}

impl Frames {
    pub const SSL_REQUEST: u32 = 80_877_103;

    pub const GSS_REQUEST: u32 = 80_877_104;

    pub fn feed(&mut self, bytes: &[u8], unframed_replies: &AtomicUsize) -> Vec<(u8, Vec<u8>)> {
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

pub fn c_string(bytes: &[u8]) -> (String, &[u8]) {
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
/// Whether a query string holds a statement that changes the catalog.
///
/// Each `;`-separated statement is classified by its first keyword after
/// leading whitespace and comments. A `DO` block counts, because its body
/// can run any DDL. Splitting on `;` also splits inside string literals,
/// which can only add a false report. What this cannot see is DDL performed
/// inside a function the statement calls; the catalog comparison the tests
/// make around the serving interval covers any lasting effect of that.
pub fn changes_catalog(sql: &str) -> bool {
    sql.split(';').any(|statement| {
        let mut rest = statement.trim_start();
        loop {
            if let Some(comment) = rest.strip_prefix("--") {
                rest = comment
                    .split_once('\n')
                    .map_or("", |(_, after)| after)
                    .trim_start();
            } else if let Some(comment) = rest.strip_prefix("/*") {
                rest = comment
                    .split_once("*/")
                    .map_or("", |(_, after)| after)
                    .trim_start();
            } else {
                break;
            }
        }
        let keyword: String = rest
            .chars()
            .take_while(|character| character.is_ascii_alphabetic())
            .collect::<String>()
            .to_ascii_uppercase();
        matches!(
            keyword.as_str(),
            "CREATE"
                | "ALTER"
                | "DROP"
                | "GRANT"
                | "REVOKE"
                | "TRUNCATE"
                | "COMMENT"
                | "REINDEX"
                | "DO"
                | "SECURITY"
                | "REASSIGN"
                | "IMPORT"
        )
    })
}

pub fn record_frontend(wire: &Mutex<Wire>, kind: u8, payload: &[u8]) {
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

pub fn record_backend(wire: &Mutex<Wire>, kind: u8, payload: &[u8]) {
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

pub async fn relay<R, W>(
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

pub enum Upstream {
    Tcp(String),
    Unix(PathBuf),
}

/// The Unix socket the runtime login connects through. Restricted startup
/// accepts unauthenticated transport only over a Unix socket, so this
/// endpoint fronts the lane's PostgreSQL whether that listens on TCP or on
/// its own socket. Every connection is relayed unchanged and recorded in
/// [`Wire`], which is how the tests see the runtime's errors and statements
/// without access to the PostgreSQL log.
pub struct SocketEndpoint {
    pub directory: tempfile::TempDir,
    pub port: u16,
    pub wire: Arc<Mutex<Wire>>,
    pub forwarder: tokio::task::JoinHandle<()>,
}

impl SocketEndpoint {
    pub async fn for_url(url: &str) -> Self {
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

    pub fn take_wire(&self) -> Wire {
        std::mem::take(&mut *self.wire.lock().expect("wire record"))
    }
}

impl Drop for SocketEndpoint {
    fn drop(&mut self) {
        self.forwarder.abort();
    }
}

pub struct Roles {
    pub database_owner: String,
    pub schema_owner: String,
    pub migrator: String,
    pub runtime: String,
    pub old_runtime: String,
    pub installer: String,
    pub definer: String,
}

impl Roles {
    pub fn all(&self) -> [&String; 7] {
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
pub struct Prepared {
    pub cluster: PgPool,
    pub admin: PgPool,
    pub database: String,
    pub roles: Roles,
    pub endpoint: SocketEndpoint,
    pub lineage: AttestedLineage,
    pub runtime_url: String,
    pub handoff: String,
    pub owner_url: String,
    pub plan: RestrictedRuntimeOwnerPlan,
    /// The attestor installation the handoff pins.
    pub operation_id: Uuid,
}

/// Roles, a database and its two schemas, before any owner phase has run.
pub struct Provisioned {
    pub cluster: PgPool,
    pub admin: PgPool,
    pub database: String,
    pub roles: Roles,
    pub base: PgConnectOptions,
    pub owner_url: String,
    pub plan: RestrictedRuntimeOwnerPlan,
}

/// Run the owner phase on a fresh handle: the migrator login assumes the
/// NOLOGIN schema owner.
pub async fn run_owner_phase(
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

pub async fn drop_fixture(cluster: PgPool, admin: PgPool, database: &str, roles: &Roles) {
    admin.close().await;
    execute(&cluster, &format!("DROP DATABASE {database} WITH (FORCE)")).await;
    for role in roles.all() {
        execute(&cluster, &format!("DROP ROLE IF EXISTS {role}")).await;
    }
    cluster.close().await;
}

impl Provisioned {
    pub async fn new(url: &str) -> Self {
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
}

impl Prepared {
    /// Run the owner phase with the provisioned plan, then install the
    /// attestor and build the runtime handoff.
    pub async fn from_provisioned(provisioned: Provisioned, url: &str) -> Self {
        let lineage = run_owner_phase(&provisioned.owner_url, &provisioned.plan)
            .await
            .expect("owner phase prepares the runtime schemas");
        Self::attest(provisioned, lineage, url).await
    }

    /// Install the attestor on a database the owner phase has prepared and
    /// build the runtime handoff for it.
    pub async fn attest(provisioned: Provisioned, lineage: AttestedLineage, url: &str) -> Self {
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
        let mut prepared = Self {
            cluster,
            admin,
            database,
            roles,
            endpoint,
            lineage,
            runtime_url,
            handoff: String::new(),
            owner_url,
            plan,
            operation_id,
        };
        prepared.handoff = prepared.handoff_for(&prepared.runtime_url).await;
        prepared
    }

    /// The operator handoff for a runtime that connects through
    /// `runtime_url`. The handoff names the endpoint, so each endpoint a
    /// node connects through has its own.
    pub async fn handoff_for(&self, runtime_url: &str) -> String {
        handoff(
            &self.admin,
            &self.owner_url,
            &self.roles,
            &self.lineage,
            runtime_url,
            self.operation_id,
        )
        .await
    }

    /// Run the owner phase again on the already prepared database.
    pub async fn rerun_owner_phase(&self) -> Result<AttestedLineage, RestrictedRuntimeOwnerError> {
        run_owner_phase(&self.owner_url, &self.plan).await
    }

    /// Environment that starts the server under the runtime login through
    /// one endpoint and its handoff.
    pub fn runtime_env_for(&self, runtime_url: &str, handoff: &str) -> Vec<(&'static str, String)> {
        vec![
            ("WADDLE_DB_DRIVER", "postgres".to_owned()),
            ("WADDLE_DATABASE_URL", runtime_url.to_owned()),
            (
                "WADDLE_DEPLOYMENT_UUID",
                self.lineage.deployment_uuid.to_string(),
            ),
            ("WADDLE_GIT_SHA", RELEASE_GIT_SHA.to_owned()),
            ("WADDLE_DB_RUNTIME_ATTESTATION", handoff.to_owned()),
            ("WADDLE_DB_MIGRATOR_LOGIN", self.roles.migrator.clone()),
            (
                "WADDLE_XMPP_MAM_SCHEMA_OWNER",
                self.roles.schema_owner.clone(),
            ),
            ("WADDLE_XMPP_MAM_CLAIM_SCHEMA", SCHEMA.to_owned()),
            ("WADDLE_SESSION_KEY", SESSION_KEY.to_owned()),
        ]
    }

    pub async fn cleanup(self) {
        drop_fixture(self.cluster, self.admin, &self.database, &self.roles).await;
        drop(self.endpoint);
    }
}

pub fn role(name: &str) -> PgRoleName {
    PgRoleName::new(name).expect("generated role name")
}

pub async fn execute(pool: &PgPool, statement: &str) {
    sqlx::query(statement)
        .execute(pool)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

pub fn url_with(base: &str, database: &str, user: &str, options: &str) -> String {
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

/// The operator's cutover record for a runtime that connects through
/// `runtime_url`: the endpoint exactly as the runtime's connection options
/// name it, the retired login, and a fresh reference for every cutoff.
pub async fn cutover_record(admin: &PgPool, old_runtime: &str, runtime_url: &str) -> String {
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
    let old_runtime_oid: i64 =
        sqlx::query_scalar("SELECT oid::bigint FROM pg_catalog.pg_roles WHERE rolname = $1")
            .bind(old_runtime)
            .fetch_one(admin)
            .await
            .expect("administrator records the retired login");
    serde_json::json!({
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
            "login": old_runtime,
            "role_oid": old_runtime_oid,
            "credential_cutoff_record_id": Uuid::new_v4().to_string(),
        },
        "listener_cutoff_record_id": Uuid::new_v4().to_string(),
        "backend_cutoff_record_id": Uuid::new_v4().to_string(),
        "prepared_transaction_cutoff_record_id": Uuid::new_v4().to_string(),
        "mutator_policy": {
            "mode": "quiesced_for_serving_interval",
            "record_id": Uuid::new_v4().to_string(),
        },
    })
    .to_string()
}

/// The operator handoff, composed the way `emit-handoff` composes it: the
/// pins are read on the owner connection, never on the runtime connection
/// the handoff will be checked against.
pub async fn handoff(
    admin: &PgPool,
    owner_url: &str,
    roles: &Roles,
    lineage: &AttestedLineage,
    runtime_url: &str,
    operation_id: Uuid,
) -> String {
    let cutover = cutover_record(admin, &roles.old_runtime, runtime_url).await;
    let mut owner_config = DatabaseConfig::new(DatabaseDriver::Postgres, owner_url.to_owned());
    owner_config.pool_size = 1;
    let owner = Database::from_config("restricted runtime handoff", &owner_config)
        .await
        .expect("owner database");
    compose_handoff(
        &owner,
        &HandoffInputs {
            release_git_sha: RELEASE_GIT_SHA,
            deployment_uuid: lineage.deployment_uuid,
            installer_login: &role(&roles.installer),
            runtime_login: &role(&roles.runtime),
            definer_role: &role(&roles.definer),
            trusted_schema: &PgSchemaName(TRUSTED_SCHEMA.to_owned()),
        },
        &OperatorRecord {
            installation_operation_id: operation_id,
            cutover: &cutover,
        },
    )
    .await
    .expect("compose the operator handoff")
    .document()
    .to_owned()
}

/// The catalog state a restricted runtime must leave alone: every relation,
/// column, routine (with its `SECURITY DEFINER` flag and configuration),
/// constraint, trigger and type in the application and attestor schemas with
/// owner and ACL, every schema, the database ACL, default privileges, and
/// the role memberships of this fixture's roles. DDL or a grant of any kind
/// changes this list. An object created and dropped again between two
/// readings does not; [`Wire`] covers that for the runtime login.
pub async fn catalog_entries(admin: &PgPool) -> Vec<String> {
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
pub async fn assert_catalog_unchanged(admin: &PgPool, before: &[String], context: &str) {
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

pub async fn probe(url: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .unwrap_or_else(|error| panic!("GET {url}: {error}"))
        .status()
}

pub const DOMAIN: &str = "localhost";
pub const NS_MUC: &str = "http://jabber.org/protocol/muc";
pub const NS_MAM: &str = "urn:xmpp:mam:2";
pub const WIRE_BUDGET: Duration = Duration::from_secs(20);

pub async fn send_element(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize stanza");
    client
        .send(&String::from_utf8(bytes).expect("stanza UTF-8"))
        .await
        .expect("send stanza");
}

pub fn iq_request(set: bool, id: &str, to: Option<&str>, payload: Element) -> Iq {
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
/// Receive frames until `done` accepts the stanzas collected so far.
pub async fn collect_until(
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

pub fn has_descendant(element: &Element, name: &str, namespace: &str) -> bool {
    element
        .children()
        .any(|child| child.is(name, namespace) || has_descendant(child, name, namespace))
}

pub fn contains_text(element: &Element, needle: &str) -> bool {
    element.text().contains(needle) || element.children().any(|child| contains_text(child, needle))
}

pub fn chat_message(to: &str, kind: MessageType, id: &str, body: &str) -> Element {
    let mut message = Message::new(Some(to.parse().expect("message recipient")));
    message.id = Some(Id(id.to_owned()));
    message.type_ = kind;
    message.bodies.insert(Lang::new(), body.to_owned());
    message.into()
}

pub fn muc_presence(occupant: &str, leave: bool) -> Element {
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
pub async fn mam_query(client: &mut WsXmppClient, id: &str, archive: Option<&str>) -> Vec<Element> {
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

/// Enable XEP-0198 with resumption, have a stanza acknowledged, lose the
/// transport without closing the stream, and resume on a new one.
pub async fn exercise_stream_management(enabled_on: &TestServer, resumed_on: &TestServer) {
    let mut client = WsXmppClient::connect(&enabled_on.ws_url())
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

    let mut resumed = WsXmppClient::connect(&resumed_on.ws_url())
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
        "the stream must resume: {response:?}"
    );
    resumed.close().await.expect("close resumed stream");
}

/// A configured keypair pool: the base64 seeds the server is started with
/// and the PeerIds the owner enrolls, in the same order.
#[cfg(feature = "clustering")]
pub struct KeypairPool {
    pub seeds: Vec<String>,
    pub peer_ids: Vec<libp2p::PeerId>,
}

#[cfg(feature = "clustering")]
pub fn keypair_pool(size: usize) -> KeypairPool {
    use base64::Engine;
    let mut seeds = Vec::new();
    let mut peer_ids = Vec::new();
    for _ in 0..size {
        let keypair = libp2p::identity::ed25519::Keypair::generate();
        seeds.push(base64::engine::general_purpose::STANDARD.encode(keypair.secret().as_ref()));
        peer_ids.push(
            libp2p::identity::Keypair::from(keypair)
                .public()
                .to_peer_id(),
        );
    }
    KeypairPool { seeds, peer_ids }
}

/// Roles and a database whose owner plan enrolls `pool` for clustering. The
/// fixture keeps its administrator session open, so the plan does not
/// require an exclusive database.
#[cfg(feature = "clustering")]
pub async fn provision_clustered(url: &str, pool: &KeypairPool) -> Provisioned {
    use waddle_server::server::runtime_owner::{ClusteringOwnerPlan, ExclusiveDatabase};
    let mut provisioned = Provisioned::new(url).await;
    provisioned.plan.clustering = Some(ClusteringOwnerPlan {
        exclusive_database: ExclusiveDatabase::NotChecked,
        ..ClusteringOwnerPlan::new(pool.peer_ids.clone(), Uuid::new_v4(), String::new())
    });
    provisioned
}
