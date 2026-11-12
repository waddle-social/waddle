//! The owner entrypoint, run as an operator runs it: the `owner-phase`,
//! `install-attestor` and `emit-handoff` subcommands of the real server
//! binary, each as its own process under its own login, followed by the real
//! server under the runtime login with the emitted handoff.
//!
//! Every subcommand's output is checked for the secrets it was given.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::db::runtime_startup::database_identity_from_json;
use waddle_ws_test_support::{require_test_postgres_url, TestServer, WsXmppClient};

use super::support::{cutover_record, drop_fixture, probe, Roles, SocketEndpoint, TRUSTED_SCHEMA};
use super::{
    execute, owner_phase_stable_entries, relation_exists, Prepared, Provisioned, DOMAIN,
    RELEASE_GIT_SHA, SCHEMA, SESSION_KEY,
};

const SUCCESS: i32 = 0;
const FAILED: i32 = 1;
const USAGE: i32 = 2;
#[cfg(feature = "clustering")]
const PENDING: i32 = 3;
const REFUSED: i32 = 4;

const MIGRATOR_PASSWORD: &str = "migrator-password-never-printed";
const INSTALLER_PASSWORD: &str = "installer-password-never-printed";
const RUNTIME_PASSWORD: &str = "runtime-password-never-printed";

/// One finished subcommand.
struct Finished {
    code: i32,
    stdout: String,
    stderr: String,
}

/// The operator's side of a cutover: the role and schema names every
/// subcommand is configured with, and the secrets none may print.
struct Operator {
    cluster_url: String,
    database: String,
    migrator: String,
    installer: String,
    env: Vec<(&'static str, String)>,
    secrets: Vec<String>,
}

impl Operator {
    fn new(
        cluster_url: &str,
        database: &str,
        roles: &Roles,
        schema: &str,
        deployment: String,
    ) -> Self {
        Self {
            cluster_url: cluster_url.to_owned(),
            database: database.to_owned(),
            migrator: roles.migrator.clone(),
            installer: roles.installer.clone(),
            env: vec![
                ("WADDLE_XMPP_MAM_CLAIM_SCHEMA", schema.to_owned()),
                ("WADDLE_DB_MIGRATOR_LOGIN", roles.migrator.clone()),
                ("WADDLE_XMPP_MAM_SCHEMA_OWNER", roles.schema_owner.clone()),
                ("WADDLE_DB_RUNTIME_LOGIN", roles.runtime.clone()),
                ("WADDLE_DB_ATTESTOR_DEFINER_ROLE", roles.definer.clone()),
                (
                    "WADDLE_DB_ATTESTOR_INSTALLER_LOGIN",
                    roles.installer.clone(),
                ),
                ("WADDLE_DB_ATTESTOR_SCHEMA", TRUSTED_SCHEMA.to_owned()),
                ("WADDLE_DEPLOYMENT_UUID", deployment),
                ("WADDLE_SESSION_KEY", SESSION_KEY.to_owned()),
            ],
            secrets: vec![
                MIGRATOR_PASSWORD.to_owned(),
                INSTALLER_PASSWORD.to_owned(),
                RUNTIME_PASSWORD.to_owned(),
                SESSION_KEY.to_owned(),
            ],
        }
    }

    fn migrator_url(&self) -> String {
        login_url(
            &self.cluster_url,
            &self.database,
            &self.migrator,
            MIGRATOR_PASSWORD,
        )
    }

    fn installer_url(&self) -> String {
        login_url(
            &self.cluster_url,
            &self.database,
            &self.installer,
            INSTALLER_PASSWORD,
        )
    }

    /// Run one subcommand of the server binary to completion and check its
    /// output for every secret.
    async fn run(&self, database_url: &str, args: &[&str], extra: &[(&str, &str)]) -> Finished {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_waddle-server"));
        for (key, _) in std::env::vars_os() {
            let key_text = key.to_string_lossy();
            if key_text.starts_with("WADDLE_")
                || key_text.starts_with("OTEL_")
                || key_text.starts_with("PG")
            {
                command.env_remove(key);
            }
        }
        command
            .args(args)
            .env("WADDLE_DATABASE_URL", database_url)
            .kill_on_drop(true);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        for (key, value) in extra {
            command.env(key, value);
        }
        let output = tokio::time::timeout(Duration::from_secs(300), command.output())
            .await
            .unwrap_or_else(|_| panic!("{args:?} did not exit"))
            .unwrap_or_else(|error| panic!("run {args:?}: {error}"));
        let finished = Finished {
            code: output.status.code().expect("subcommand exit code"),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        };
        let urls = extra
            .iter()
            .filter(|(key, _)| key.ends_with("_URL"))
            .map(|(_, value)| *value)
            .chain([database_url]);
        for secret in self.secrets.iter().map(String::as_str).chain(urls) {
            assert!(
                !finished.stdout.contains(secret) && !finished.stderr.contains(secret),
                "{args:?} printed a secret or a connection URL"
            );
        }
        finished
    }

    async fn owner_phase(&self, args: &[&str], extra: &[(&str, &str)]) -> Finished {
        let mut all = vec!["owner-phase"];
        all.extend_from_slice(args);
        self.run(&self.migrator_url(), &all, extra).await
    }

    async fn install_attestor(&self, identity: &Path, operation_id: Uuid) -> Finished {
        let operation_id = operation_id.to_string();
        self.run(
            &self.installer_url(),
            &[
                "install-attestor",
                "--identity",
                identity.to_str().expect("identity path"),
                "--operation-id",
                &operation_id,
            ],
            &[],
        )
        .await
    }

    async fn emit_handoff(&self, args: &[&str], extra: &[(&str, &str)]) -> Finished {
        let mut all = vec!["emit-handoff"];
        all.extend_from_slice(args);
        self.run(&self.migrator_url(), &all, extra).await
    }
}

/// `base` pointed at `database` as `login`, with a password the trust
/// authentication of the test cluster ignores.
fn login_url(base: &str, database: &str, login: &str, password: &str) -> String {
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
        .append_pair("user", login)
        .append_pair("password", password);
    url.to_string()
}

/// The runtime login's URL through `endpoint`, with `schema` as its search
/// path.
fn runtime_url(endpoint: &SocketEndpoint, database: &str, runtime: &str, schema: &str) -> String {
    format!(
        "postgresql:///{database}?host={}&port={}&user={runtime}&password={RUNTIME_PASSWORD}\
         &options=-c%20search_path%3D{schema}",
        endpoint.directory.path().display(),
        endpoint.port,
    )
}

/// The one-time provisioning the planning document gives for a database a
/// single login owns.
const PROVISIONING: &str = include_str!("../../sql/restricted_runtime_provisioning.sql");

/// Run the provisioning procedure exactly as written, as a superuser on one
/// session, with its parameters taken from `roles` and then `overrides`.
async fn provision(
    superuser: &PgConnectOptions,
    schema: &str,
    roles: &Roles,
    overrides: &[(&str, &str)],
) -> Result<(), sqlx::Error> {
    let mut session = PgConnection::connect_with(superuser).await?;
    let parameters = [
        ("waddle.schema", schema),
        ("waddle.old_login", roles.old_runtime.as_str()),
        ("waddle.database_owner", roles.database_owner.as_str()),
        ("waddle.schema_owner", roles.schema_owner.as_str()),
        ("waddle.migrator_login", roles.migrator.as_str()),
        ("waddle.runtime_login", roles.runtime.as_str()),
        ("waddle.attestor_installer_login", roles.installer.as_str()),
        ("waddle.attestor_definer_role", roles.definer.as_str()),
        ("waddle.attestor_schema", TRUSTED_SCHEMA),
    ];
    for (setting, value) in parameters.iter().chain(overrides) {
        sqlx::query("SELECT pg_catalog.set_config($1, $2, false)")
            .bind(setting)
            .bind(value)
            .execute(&mut session)
            .await?;
    }
    let outcome = sqlx::raw_sql(PROVISIONING).execute(&mut session).await;
    // A refused run leaves its transaction open; closing the session ends it.
    session.close().await?;
    outcome.map(|_| ())
}

/// Everything the provisioning procedure can change, as a superuser reads
/// it: the attributes, password presence, memberships and settings of
/// `role_names`, the database's owner and ACL, every schema with its owner
/// and ACL, every relation and routine outside the system schemas with its
/// owner and ACL, the ACL of `pg_control_system()` and all default
/// privileges.
async fn provisioning_snapshot(superuser: &PgConnectOptions, role_names: &[&str]) -> Vec<String> {
    let mut session = PgConnection::connect_with(superuser)
        .await
        .expect("superuser session");
    let role_names: Vec<String> = role_names.iter().map(|name| (*name).to_owned()).collect();
    let entries = sqlx::query_scalar(
        "SELECT entry FROM ( \
           SELECT pg_catalog.format('role %s super=%s inherit=%s createrole=%s createdb=%s \
                    login=%s replication=%s bypassrls=%s password=%s', rolname, rolsuper, \
                    rolinherit, rolcreaterole, rolcreatedb, rolcanlogin, rolreplication, \
                    rolbypassrls, rolpassword IS NOT NULL) AS entry \
             FROM pg_catalog.pg_authid WHERE rolname = ANY($1) \
           UNION ALL \
           SELECT pg_catalog.format('member %s of %s admin=%s inherit=%s set=%s', \
                    pg_catalog.pg_get_userbyid(member), pg_catalog.pg_get_userbyid(roleid), \
                    admin_option, inherit_option, set_option) \
             FROM pg_catalog.pg_auth_members \
            WHERE pg_catalog.pg_get_userbyid(member) = ANY($1) \
               OR pg_catalog.pg_get_userbyid(roleid) = ANY($1) \
           UNION ALL \
           SELECT pg_catalog.format('setting %s %s', pg_catalog.pg_get_userbyid(setrole), setconfig) \
             FROM pg_catalog.pg_db_role_setting \
            WHERE setrole <> 0 AND pg_catalog.pg_get_userbyid(setrole) = ANY($1) \
           UNION ALL \
           SELECT pg_catalog.format('database %s %s', pg_catalog.pg_get_userbyid(datdba), datacl) \
             FROM pg_catalog.pg_database WHERE datname = pg_catalog.current_database() \
           UNION ALL \
           SELECT pg_catalog.format('schema %s %s %s', nspname, \
                    pg_catalog.pg_get_userbyid(nspowner), nspacl) \
             FROM pg_catalog.pg_namespace \
            WHERE nspname NOT LIKE 'pg\\_%' AND nspname <> 'information_schema' \
           UNION ALL \
           SELECT pg_catalog.format('relation %s.%s %s %s', n.nspname, c.relname, \
                    pg_catalog.pg_get_userbyid(c.relowner), c.relacl) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
            WHERE n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema' \
           UNION ALL \
           SELECT pg_catalog.format('routine %s.%s %s %s', n.nspname, p.proname, \
                    pg_catalog.pg_get_userbyid(p.proowner), p.proacl) \
             FROM pg_catalog.pg_proc p \
             JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
            WHERE (n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema') \
               OR p.oid = 'pg_catalog.pg_control_system()'::pg_catalog.regprocedure \
           UNION ALL \
           SELECT pg_catalog.format('default_acl %s %s %s %s', \
                    pg_catalog.pg_get_userbyid(defaclrole), defaclnamespace, defaclobjtype, \
                    defaclacl) \
             FROM pg_catalog.pg_default_acl \
         ) entries ORDER BY entry",
    )
    .bind(&role_names)
    .fetch_all(&mut session)
    .await
    .expect("provisioning snapshot");
    session.close().await.expect("close the superuser session");
    entries
}

fn assert_exit(finished: &Finished, expected: i32, context: &str) {
    assert_eq!(
        finished.code, expected,
        "{context}: stdout {:?}, stderr {:?}",
        finished.stdout, finished.stderr
    );
}

async fn start_server(env: Vec<(&'static str, String)>) -> TestServer {
    tokio::task::spawn_blocking(move || {
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
    .expect("server starts")
}

async fn assert_serves(server: &TestServer) {
    for path in ["healthz", "readyz"] {
        assert_eq!(
            probe(&format!("{}/{path}", server.http_base_url())).await,
            reqwest::StatusCode::OK,
            "/{path}"
        );
    }
}

/// The three subcommands prepare a fresh database with separate roles, and
/// the server then serves on it under the runtime login. A repeated owner
/// phase changes nothing, a handoff for another release is refused by the
/// server, and `--verify` names a pin that no longer matches the database.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subcommands_prepare_a_database_the_restricted_server_serves() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let provisioned = Provisioned::new(&url).await;
    let deployment = provisioned
        .plan
        .lineage
        .deployment_uuid
        .expect("fixture deployment identity")
        .to_string();
    let operator = Operator::new(
        &url,
        &provisioned.database,
        &provisioned.roles,
        SCHEMA,
        deployment,
    );
    let files = tempfile::tempdir().expect("operator files");
    let identity_path = files.path().join("identity.json");
    let identity_arg = identity_path.to_str().expect("identity path");
    let enroll = [("WADDLE_DB_LINEAGE_ACTION", "enroll")];

    // The parser answers before any connection is made.
    let help = operator
        .run("postgres://nowhere.invalid/none", &["--help"], &[])
        .await;
    assert_exit(&help, SUCCESS, "--help");
    for subcommand in ["owner-phase", "install-attestor", "emit-handoff"] {
        assert!(help.stdout.contains(subcommand), "{}", help.stdout);
    }
    let unknown = operator
        .run("postgres://nowhere.invalid/none", &["serve"], &[])
        .await;
    assert_exit(&unknown, USAGE, "an unknown subcommand");
    // A URL for another login is refused before it is used.
    let wrong_login = operator
        .run(&operator.installer_url(), &["owner-phase"], &enroll)
        .await;
    assert_exit(&wrong_login, USAGE, "owner-phase as the installer login");
    // The subcommand sets the session's role, search path and name. A URL
    // or an environment that would set them is refused before connecting.
    for key in ["options", "options[role]", "application_name"] {
        let mut smuggling = url::Url::parse(&operator.migrator_url()).expect("migrator URL");
        smuggling
            .query_pairs_mut()
            .append_pair(key, "smuggled_value");
        let refused = operator
            .run(smuggling.as_str(), &["owner-phase"], &enroll)
            .await;
        assert_exit(&refused, USAGE, key);
        assert!(
            refused.stderr.contains("must not set"),
            "{}",
            refused.stderr
        );
    }
    for variable in ["PGOPTIONS", "PGAPPNAME"] {
        let refused = operator
            .owner_phase(&[], &[enroll[0], (variable, "-c role=postgres")])
            .await;
        assert_exit(&refused, USAGE, variable);
        assert!(refused.stderr.contains(variable), "{}", refused.stderr);
    }
    assert!(!relation_exists(&provisioned.admin, "_lineage").await);

    let first = operator
        .owner_phase(&["--identity-output", identity_arg], &enroll)
        .await;
    assert_exit(&first, SUCCESS, "owner-phase on a fresh database");
    let identity = std::fs::read_to_string(&identity_path).expect("identity document");
    assert_eq!(first.stdout.trim(), identity.trim());
    let lineage = database_identity_from_json(&identity).expect("identity document parses");

    // A second run finds nothing to do.
    let stable = owner_phase_stable_entries(&provisioned.admin).await;
    let second = operator.owner_phase(&[], &enroll).await;
    assert_exit(&second, SUCCESS, "owner-phase on a prepared database");
    assert_eq!(second.stdout.trim(), identity.trim());
    assert_eq!(
        owner_phase_stable_entries(&provisioned.admin).await,
        stable,
        "a repeated owner-phase changed the catalog"
    );

    let operation_id = Uuid::new_v4();
    let operation_text = operation_id.to_string();
    let install_arguments = [
        "install-attestor",
        "--identity",
        identity_arg,
        "--operation-id",
        operation_text.as_str(),
    ];
    for key in ["options", "options[search_path]", "application_name"] {
        let mut smuggling = url::Url::parse(&operator.installer_url()).expect("installer URL");
        smuggling
            .query_pairs_mut()
            .append_pair(key, "smuggled_value");
        let refused = operator
            .run(smuggling.as_str(), &install_arguments, &[])
            .await;
        assert_exit(&refused, USAGE, key);
        assert!(
            refused.stderr.contains("must not set"),
            "{}",
            refused.stderr
        );
    }
    for variable in ["PGOPTIONS", "PGAPPNAME"] {
        let refused = operator
            .run(
                &operator.installer_url(),
                &install_arguments,
                &[(variable, "-c search_path=elsewhere")],
            )
            .await;
        assert_exit(&refused, USAGE, variable);
        assert!(refused.stderr.contains(variable), "{}", refused.stderr);
    }
    for attempt in ["first", "repeated"] {
        let installed = operator
            .install_attestor(&identity_path, operation_id)
            .await;
        assert_exit(&installed, SUCCESS, attempt);
        assert_eq!(installed.stdout.trim(), operation_id.to_string());
    }

    let endpoint = SocketEndpoint::for_url(&url).await;
    let runtime_url = runtime_url(
        &endpoint,
        &provisioned.database,
        &provisioned.roles.runtime,
        SCHEMA,
    );
    let cutover_path = files.path().join("cutover.json");
    std::fs::write(
        &cutover_path,
        cutover_record(
            &provisioned.admin,
            &provisioned.roles.old_runtime,
            &runtime_url,
        )
        .await,
    )
    .expect("cutover record");
    let cutover_arg = cutover_path.to_str().expect("cutover path");
    let handoff_path = files.path().join("handoff.json");
    let handoff_arg = handoff_path.to_str().expect("handoff path");
    let operation_arg = operation_id.to_string();
    let preflight_url = runtime_url.clone();
    let preflight = [("WADDLE_RUNTIME_DATABASE_URL", preflight_url.as_str())];

    // An operation ID the attestor was not installed under is refused.
    let other_operation = Uuid::new_v4().to_string();
    let mismatched = operator
        .emit_handoff(
            &[
                "--release-git-sha",
                RELEASE_GIT_SHA,
                "--installation-operation-id",
                &other_operation,
                "--cutover",
                cutover_arg,
            ],
            &[],
        )
        .await;
    assert_exit(&mismatched, REFUSED, "a handoff for another installation");
    assert!(
        mismatched.stderr.contains("was not installed by operation"),
        "{}",
        mismatched.stderr
    );

    let emitted = operator
        .emit_handoff(
            &[
                "--release-git-sha",
                RELEASE_GIT_SHA,
                "--installation-operation-id",
                &operation_arg,
                "--cutover",
                cutover_arg,
                "--output",
                handoff_arg,
            ],
            &preflight,
        )
        .await;
    assert_exit(&emitted, SUCCESS, "emit-handoff with the pinned preflight");
    let handoff = std::fs::read_to_string(&handoff_path).expect("handoff document");

    let Provisioned {
        cluster,
        admin,
        database,
        roles,
        base: _,
        owner_url,
        plan,
    } = provisioned;
    let prepared = Prepared {
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
        operation_id,
    };

    let server = start_server(prepared.runtime_env()).await;
    assert_serves(&server).await;
    let alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        "alice-password",
        "laptop",
    )
    .await
    .expect("alice authenticates and binds under the runtime role");
    alice.close().await.expect("alice closes");
    drop(server);

    // A handoff composed for another release parses for that release only:
    // the server started as this release refuses it before it listens.
    let other_release = "89abcdef0123456789abcdef0123456789abcdef";
    let for_other_release = operator
        .emit_handoff(
            &[
                "--release-git-sha",
                other_release,
                "--installation-operation-id",
                &operation_arg,
                "--cutover",
                cutover_arg,
            ],
            &[],
        )
        .await;
    assert_exit(&for_other_release, SUCCESS, "a handoff for another release");
    let (output, listener_published) = prepared
        .refused_startup(&[("WADDLE_DB_RUNTIME_ATTESTATION", &for_other_release.stdout)])
        .await;
    assert!(
        output.contains("release identity differs from WADDLE_GIT_SHA"),
        "the server must refuse a handoff for another release: {output}"
    );
    assert!(!listener_published);

    // The stored handoff still matches; the other release's does not.
    let verified = operator
        .emit_handoff(
            &[
                "--release-git-sha",
                RELEASE_GIT_SHA,
                "--verify",
                handoff_arg,
            ],
            &preflight,
        )
        .await;
    assert_exit(&verified, SUCCESS, "--verify on an unchanged database");
    assert!(verified.stdout.is_empty(), "--verify emits nothing");
    let stale_release = operator
        .emit_handoff(
            &["--release-git-sha", other_release, "--verify", handoff_arg],
            &[],
        )
        .await;
    assert_exit(&stale_release, REFUSED, "--verify for another release");
    assert!(
        stale_release.stderr.contains("release_git_sha"),
        "{}",
        stale_release.stderr
    );

    // Recreating a pinned index gives it a new OID: the stored handoff no
    // longer describes the database, and the pin is named.
    let (constraint, definition): (String, String) = sqlx::query_as(
        "SELECT conname::text, pg_catalog.pg_get_constraintdef(oid) \
           FROM pg_catalog.pg_constraint \
          WHERE conrelid = $1::regclass AND contype = 'p'",
    )
    .bind(format!("{SCHEMA}._migrations"))
    .fetch_one(&prepared.admin)
    .await
    .expect("migration ledger primary key");
    execute(
        &prepared.admin,
        &format!(
            "ALTER TABLE {SCHEMA}._migrations DROP CONSTRAINT {constraint}, \
             ADD CONSTRAINT {constraint} {definition}"
        ),
    )
    .await;
    let drifted = operator
        .emit_handoff(
            &[
                "--release-git-sha",
                RELEASE_GIT_SHA,
                "--verify",
                handoff_arg,
            ],
            &[],
        )
        .await;
    assert_exit(&drifted, REFUSED, "--verify on a drifted database");
    assert!(
        drifted.stderr.contains("pins.ledger_primary_index_oid"),
        "{}",
        drifted.stderr
    );

    prepared.cleanup().await;
}

/// The provisioning procedure never alters a role that already exists. It
/// refuses, before its first change, a privileged or system role, a role
/// whose shape differs from the one it would create, repeated parameters,
/// default privileges and a connected old login. Logins that were created
/// beforehand with a password and the right shape are accepted, and a
/// second run changes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provisioning_refuses_what_it_would_rewrite_and_repeats_without_change() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let cluster = PgPool::connect(&url).await.expect("cluster administrator");
    let suffix = Uuid::new_v4().simple().to_string();
    let database = format!("provisioning_db_{suffix}");
    let roles = Roles {
        database_owner: format!("provisioning_dbowner_{suffix}"),
        schema_owner: format!("provisioning_owner_{suffix}"),
        migrator: format!("provisioning_migrator_{suffix}"),
        runtime: format!("provisioning_runtime_{suffix}"),
        old_runtime: format!("provisioning_app_{suffix}"),
        installer: format!("provisioning_installer_{suffix}"),
        definer: format!("provisioning_definer_{suffix}"),
    };
    let superuser_role = format!("provisioning_admin_{suffix}");
    let replication_role = format!("provisioning_replica_{suffix}");
    for statement in [
        format!("CREATE ROLE {} LOGIN", roles.old_runtime),
        format!("CREATE DATABASE {database} OWNER {}", roles.old_runtime),
        format!("CREATE ROLE {superuser_role} SUPERUSER LOGIN"),
        format!("CREATE ROLE {replication_role} REPLICATION LOGIN"),
        // A login someone created with defaults: it inherits, carries a
        // setting and is a member of the old login.
        format!(
            "CREATE ROLE {} LOGIN PASSWORD 'runtime-credential'",
            roles.runtime
        ),
        format!("ALTER ROLE {} SET work_mem = '8MB'", roles.runtime),
        format!("GRANT {} TO {}", roles.old_runtime, roles.runtime),
    ] {
        execute(&cluster, &statement).await;
    }
    let superuser = PgConnectOptions::from_str(&url)
        .expect("PostgreSQL test URL")
        .database(&database);
    let old_login = superuser.clone().username(&roles.old_runtime);
    let mut application = PgConnection::connect_with(&old_login)
        .await
        .expect("old login connects");
    sqlx::query("CREATE TABLE public.carried (id integer PRIMARY KEY)")
        .execute(&mut application)
        .await
        .expect("the old login creates a relation in public");
    let mut watched: Vec<&str> = roles.all().map(String::as_str).to_vec();
    watched.extend([superuser_role.as_str(), replication_role.as_str()]);
    let untouched = provisioning_snapshot(&superuser, &watched).await;

    // The old login is still connected.
    let refusal = |outcome: Result<(), sqlx::Error>| {
        outcome.expect_err("provisioning must refuse").to_string()
    };
    let connected = refusal(provision(&superuser, "public", &roles, &[]).await);
    assert!(connected.contains("still has backends"), "{connected}");
    application.close().await.expect("old login disconnects");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while sqlx::query_scalar::<_, i64>(
        "SELECT pg_catalog.count(*) FROM pg_catalog.pg_stat_activity WHERE usename = $1",
    )
    .bind(&roles.old_runtime)
    .fetch_one(&cluster)
    .await
    .expect("count the old login's sessions")
        > 0
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the old login's session did not end"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    for (overrides, expected) in [
        (
            vec![("waddle.database_owner", superuser_role.as_str())],
            "SUPERUSER, REPLICATION or BYPASSRLS",
        ),
        (
            vec![("waddle.database_owner", replication_role.as_str())],
            "SUPERUSER, REPLICATION or BYPASSRLS",
        ),
        (
            vec![("waddle.database_owner", "pg_monitor")],
            "is a system role",
        ),
        (
            vec![("waddle.schema_owner", "postgres")],
            "is a system role",
        ),
        (
            vec![("waddle.migrator_login", roles.runtime.as_str())],
            "seven different roles",
        ),
        (vec![("waddle.attestor_schema", "public")], "must differ"),
        // The runtime login that exists has another shape.
        (vec![], "does not have the shape this procedure creates"),
    ] {
        let message = refusal(provision(&superuser, "public", &roles, &overrides).await);
        assert!(
            message.contains(expected) && message.contains("nothing was changed"),
            "{overrides:?}: {message}"
        );
        assert_eq!(
            provisioning_snapshot(&superuser, &watched).await,
            untouched,
            "a refused run with {overrides:?} changed something"
        );
    }
    let mismatched = refusal(provision(&superuser, "public", &roles, &[]).await);
    for difference in [
        "it is INHERIT",
        "it has ALTER ROLE ... SET settings",
        "is a member of",
    ] {
        assert!(mismatched.contains(difference), "{mismatched}");
    }

    // The three logins as an operator creates them beforehand: the target
    // shape, each with its own password.
    for statement in [
        format!("REVOKE {} FROM {}", roles.old_runtime, roles.runtime),
        format!("ALTER ROLE {} RESET ALL", roles.runtime),
        format!("ALTER ROLE {} NOINHERIT", roles.runtime),
        format!(
            "CREATE ROLE {} LOGIN NOINHERIT PASSWORD 'migrator-credential'",
            roles.migrator
        ),
        format!(
            "CREATE ROLE {} LOGIN NOINHERIT PASSWORD 'installer-credential'",
            roles.installer
        ),
    ] {
        execute(&cluster, &statement).await;
    }
    let login_lines = |snapshot: &[String]| -> Vec<String> {
        snapshot
            .iter()
            .filter(|entry| {
                [&roles.migrator, &roles.runtime, &roles.installer]
                    .iter()
                    .any(|login| entry.starts_with(&format!("role {login} ")))
            })
            .cloned()
            .collect()
    };
    let before = provisioning_snapshot(&superuser, &watched).await;

    // Default privileges stop the procedure, for the old login and for the
    // application schema alike, and nothing has changed when it stops.
    let mut session = PgConnection::connect_with(&superuser)
        .await
        .expect("superuser session");
    sqlx::query(&format!(
        "ALTER DEFAULT PRIVILEGES FOR ROLE {} IN SCHEMA public GRANT SELECT ON TABLES TO PUBLIC",
        roles.old_runtime
    ))
    .execute(&mut session)
    .await
    .expect("add a default privilege");
    let with_default = provisioning_snapshot(&superuser, &watched).await;
    let stopped = refusal(provision(&superuser, "public", &roles, &[]).await);
    assert!(
        stopped.contains("default privileges must be removed first")
            && stopped.contains(&roles.old_runtime)
            && stopped.contains("nothing was changed"),
        "{stopped}"
    );
    assert_eq!(
        provisioning_snapshot(&superuser, &watched).await,
        with_default
    );
    sqlx::query(&format!(
        "ALTER DEFAULT PRIVILEGES FOR ROLE {} IN SCHEMA public REVOKE SELECT ON TABLES FROM PUBLIC",
        roles.old_runtime
    ))
    .execute(&mut session)
    .await
    .expect("remove the default privilege");
    session.close().await.expect("close the superuser session");
    assert_eq!(provisioning_snapshot(&superuser, &watched).await, before);

    provision(&superuser, "public", &roles, &[])
        .await
        .expect("provisioning accepts logins that already have the target shape");
    let provisioned = provisioning_snapshot(&superuser, &watched).await;
    assert_eq!(
        login_lines(&provisioned),
        login_lines(&before),
        "pre-existing logins must keep their attributes and passwords"
    );
    assert_eq!(login_lines(&provisioned).len(), 3);
    assert!(
        login_lines(&provisioned)
            .iter()
            .all(|entry| entry.ends_with("password=t")),
        "{provisioned:#?}"
    );
    for privileged in [&superuser_role, &replication_role] {
        let line = |snapshot: &[String]| {
            snapshot
                .iter()
                .find(|entry| entry.starts_with(&format!("role {privileged} ")))
                .cloned()
        };
        assert_eq!(line(&provisioned), line(&untouched));
    }
    assert!(
        provisioned.contains(&format!("relation public.carried {} ", roles.schema_owner)),
        "the old login's relation must belong to the schema owner: {provisioned:#?}"
    );

    provision(&superuser, "public", &roles, &[])
        .await
        .expect("repeated provisioning");
    assert_eq!(
        provisioning_snapshot(&superuser, &watched).await,
        provisioned,
        "a repeated provisioning run changed the catalog"
    );

    let admin = PgPool::connect_with(superuser)
        .await
        .expect("database administrator");
    drop_fixture(cluster.clone(), admin, &database, &roles).await;
    let cluster = PgPool::connect(&url).await.expect("cluster administrator");
    for role in [&superuser_role, &replication_role] {
        execute(&cluster, &format!("DROP ROLE {role}")).await;
    }
    cluster.close().await;
}

/// A PostgreSQL server of this test's own, reachable only through a Unix
/// socket in a private directory, that authenticates every connection by
/// password.
struct PasswordCluster {
    directory: tempfile::TempDir,
    server: tokio::process::Child,
}

const CLUSTER_PASSWORD: &str = "cluster-administrator-password-never-printed";

impl PasswordCluster {
    /// `None` when the PostgreSQL server programs are not on `PATH`.
    async fn start() -> Option<Self> {
        let program = |name: &str| {
            std::env::split_paths(&std::env::var_os("PATH")?)
                .map(|directory| directory.join(name))
                .find(|candidate| candidate.is_file())
        };
        let (initdb, postgres) = (program("initdb")?, program("postgres")?);
        let directory = tempfile::Builder::new()
            .prefix("wpc")
            .tempdir()
            .expect("cluster directory");
        let data = directory.path().join("data");
        let password_file = directory.path().join("password");
        std::fs::write(&password_file, CLUSTER_PASSWORD).expect("password file");
        let initialized = tokio::process::Command::new(initdb)
            .arg("--pgdata")
            .arg(&data)
            .args(["--username", "postgres", "--auth", "scram-sha-256"])
            .args(["--encoding", "UTF8", "--no-sync", "--pwfile"])
            .arg(&password_file)
            .output()
            .await
            .expect("run initdb");
        assert!(
            initialized.status.success(),
            "initdb: {}",
            String::from_utf8_lossy(&initialized.stderr)
        );
        let server = tokio::process::Command::new(postgres)
            .arg("-D")
            .arg(&data)
            .arg("-k")
            .arg(directory.path())
            .args(["-c", "listen_addresses=", "-c", "fsync=off"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("start postgres");
        let cluster = Self { directory, server };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            match PgConnection::connect(&cluster.url()).await {
                Ok(session) => {
                    session.close().await.expect("close the readiness session");
                    return Some(cluster);
                }
                Err(error) => assert!(
                    tokio::time::Instant::now() < deadline,
                    "the private cluster did not accept connections: {error}"
                ),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn url(&self) -> String {
        format!(
            "postgresql:///postgres?host={}&port=5432&user=postgres&password={CLUSTER_PASSWORD}",
            self.directory.path().display()
        )
    }

    async fn stop(mut self) {
        self.server.kill().await.expect("stop the private cluster");
    }
}

/// Under password authentication no subcommand echoes a password or a
/// connection URL: not when authentication fails, not when the connection
/// cannot be made, and not when the pinned preflight fails on the runtime
/// connection it was given.
///
/// The shared test server authenticates nothing, so this test starts its
/// own. It is skipped when `initdb` and `postgres` are not on `PATH`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subcommands_do_not_echo_credentials_under_password_authentication() {
    let Some(cluster) = PasswordCluster::start().await else {
        eprintln!("skipped: initdb and postgres are not on PATH");
        return;
    };
    let url = cluster.url();
    let provisioned = Provisioned::new(&url).await;
    for (login, password) in [
        (&provisioned.roles.migrator, MIGRATOR_PASSWORD),
        (&provisioned.roles.installer, INSTALLER_PASSWORD),
        (&provisioned.roles.runtime, RUNTIME_PASSWORD),
    ] {
        execute(
            &provisioned.cluster,
            &format!("ALTER ROLE {login} PASSWORD '{password}'"),
        )
        .await;
    }
    let deployment = provisioned
        .plan
        .lineage
        .deployment_uuid
        .expect("fixture deployment identity")
        .to_string();
    let mut operator = Operator::new(
        &url,
        &provisioned.database,
        &provisioned.roles,
        SCHEMA,
        deployment,
    );
    const WRONG_PASSWORD: &str = "wrong-password-never-printed";
    operator
        .secrets
        .extend([CLUSTER_PASSWORD.to_owned(), WRONG_PASSWORD.to_owned()]);
    let wrong = |login: &str| login_url(&url, &provisioned.database, login, WRONG_PASSWORD);
    let files = tempfile::tempdir().expect("operator files");
    let identity_path = files.path().join("identity.json");
    let identity_arg = identity_path.to_str().expect("identity path");
    let enroll = [("WADDLE_DB_LINEAGE_ACTION", "enroll")];
    let owner_phase = ["owner-phase", "--identity-output", identity_arg];

    // Authentication fails.
    let rejected = operator
        .run(&wrong(&provisioned.roles.migrator), &owner_phase, &enroll)
        .await;
    assert_exit(&rejected, FAILED, "owner-phase with a wrong password");
    assert!(
        rejected.stderr.contains("password authentication failed"),
        "{}",
        rejected.stderr
    );
    // The connection cannot be made at all.
    let nowhere = format!(
        "postgresql:///{}?host={}&port=5432&user={}&password={MIGRATOR_PASSWORD}",
        provisioned.database,
        files.path().join("no-server-here").display(),
        provisioned.roles.migrator
    );
    let unreachable = operator.run(&nowhere, &owner_phase, &enroll).await;
    assert_exit(&unreachable, FAILED, "owner-phase without a server");

    let prepared = operator
        .run(&operator.migrator_url(), &owner_phase, &enroll)
        .await;
    assert_exit(&prepared, SUCCESS, "owner-phase with its password");

    let operation_id = Uuid::new_v4();
    let operation_arg = operation_id.to_string();
    let install = [
        "install-attestor",
        "--identity",
        identity_arg,
        "--operation-id",
        operation_arg.as_str(),
    ];
    let rejected = operator
        .run(&wrong(&provisioned.roles.installer), &install, &[])
        .await;
    assert_exit(&rejected, FAILED, "install-attestor with a wrong password");
    assert_exit(
        &operator
            .install_attestor(&identity_path, operation_id)
            .await,
        SUCCESS,
        "install-attestor with its password",
    );

    let endpoint = SocketEndpoint::for_url(&url).await;
    let runtime_url = runtime_url(
        &endpoint,
        &provisioned.database,
        &provisioned.roles.runtime,
        SCHEMA,
    );
    let cutover_path = files.path().join("cutover.json");
    std::fs::write(
        &cutover_path,
        cutover_record(
            &provisioned.admin,
            &provisioned.roles.old_runtime,
            &runtime_url,
        )
        .await,
    )
    .expect("cutover record");
    let emit = [
        "emit-handoff",
        "--release-git-sha",
        RELEASE_GIT_SHA,
        "--installation-operation-id",
        operation_arg.as_str(),
        "--cutover",
        cutover_path.to_str().expect("cutover path"),
    ];
    let rejected = operator
        .run(&wrong(&provisioned.roles.migrator), &emit, &[])
        .await;
    assert_exit(&rejected, FAILED, "emit-handoff with a wrong password");

    // The preflight is given a runtime URL whose password is wrong, then
    // the right one.
    let wrong_runtime = runtime_url.replace(RUNTIME_PASSWORD, WRONG_PASSWORD);
    let failed_preflight = operator
        .run(
            &operator.migrator_url(),
            &emit,
            &[("WADDLE_RUNTIME_DATABASE_URL", wrong_runtime.as_str())],
        )
        .await;
    assert_ne!(
        failed_preflight.code, SUCCESS,
        "a preflight that cannot authenticate must fail: {}",
        failed_preflight.stderr
    );
    assert!(
        failed_preflight.stdout.is_empty(),
        "no handoff is emitted when the preflight fails"
    );
    let emitted = operator
        .run(
            &operator.migrator_url(),
            &emit,
            &[("WADDLE_RUNTIME_DATABASE_URL", runtime_url.as_str())],
        )
        .await;
    assert_exit(&emitted, SUCCESS, "emit-handoff with the pinned preflight");

    drop(endpoint);
    provisioned.cleanup().await;
    cluster.stop().await;
}

#[cfg(feature = "clustering")]
mod clustered {
    use std::str::FromStr;
    use std::time::Duration;

    use minidom::Element;
    use sqlx::postgres::{PgConnectOptions, PgPool};
    use uuid::Uuid;
    use waddle_server::clustering::allowlist::{AllowlistStore, PostgresAllowlistStore};
    use waddle_server::clustering::lease::PostgresKeypairSlotLease;
    use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
    use waddle_ws_test_support::{require_test_postgres_url, TestServer, WsXmppClient};
    use xmpp_parsers::message::MessageType;

    use super::super::support::{
        chat_message, collect_until, contains_text, cutover_record, drop_fixture, has_descendant,
        keypair_pool, mam_query, muc_presence, send_element, KeypairPool, Roles, SocketEndpoint,
    };
    use super::super::{
        execute, exercise_roster, iq_request, iq_result, relation_exists, Provisioned, DOMAIN,
        NS_ROSTER, RELEASE_GIT_SHA, SCHEMA, SESSION_KEY,
    };
    use super::{
        assert_exit, assert_serves, login_url, provision, provisioning_snapshot, runtime_url,
        Operator, PENDING, REFUSED, SUCCESS,
    };

    fn clustered_operator(
        url: &str,
        database: &str,
        roles: &Roles,
        schema: &str,
        deployment: String,
        keypairs: &KeypairPool,
    ) -> Operator {
        let mut operator = Operator::new(url, database, roles, schema, deployment);
        operator.env.extend([
            ("WADDLE_CLUSTERING_ENABLED", "true".to_owned()),
            ("WADDLE_CLUSTERING_KEYPAIR_POOL", keypairs.seeds.join(",")),
        ]);
        operator.secrets.extend(keypairs.seeds.iter().cloned());
        operator
    }

    async fn admin_pool(provisioned: &Provisioned) -> PgPool {
        PgPool::connect_with(provisioned.base.clone().database(&provisioned.database))
            .await
            .expect("database administrator")
    }

    /// A clustering plan reports a pending generation with its own exit code
    /// and the activation time, succeeds once the delay has passed, and
    /// refuses with the reason when another session is connected or the
    /// keypair-slot table is one an unattested server created.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn clustered_owner_phase_reports_pending_activation_and_refusals() {
        let Some(url) = require_test_postgres_url() else {
            return;
        };
        let keypairs = keypair_pool(2);
        let enroll = [("WADDLE_DB_LINEAGE_ACTION", "enroll")];

        let mut provisioned = Provisioned::new(&url).await;
        let deployment = provisioned
            .plan
            .lineage
            .deployment_uuid
            .expect("fixture deployment identity")
            .to_string();
        let operator = clustered_operator(
            &url,
            &provisioned.database,
            &provisioned.roles,
            SCHEMA,
            deployment,
            &keypairs,
        );
        let generation = Uuid::new_v4().to_string();
        let plan = [
            "--keypool-generation",
            generation.as_str(),
            "--keypool-activation-delay",
            "2s",
        ];

        // Clustering without a generation is a usage error.
        let incomplete = operator.owner_phase(&[], &enroll).await;
        assert_eq!(incomplete.code, super::USAGE, "{}", incomplete.stderr);
        assert!(
            incomplete.stderr.contains("--keypool-generation"),
            "{}",
            incomplete.stderr
        );

        // The fixture's administrator is connected: the default plan needs
        // the database to itself, names the session and changes nothing.
        let crowded = operator.owner_phase(&plan, &enroll).await;
        assert_exit(&crowded, REFUSED, "owner-phase beside another session");
        assert!(
            crowded.stderr.contains("needs the database to itself")
                && crowded.stderr.contains("other session pid="),
            "{}",
            crowded.stderr
        );
        assert!(!relation_exists(&provisioned.admin, "_lineage").await);

        provisioned.admin.close().await;
        let pending = operator.owner_phase(&plan, &enroll).await;
        assert_exit(&pending, PENDING, "owner-phase inside the activation delay");
        assert!(
            pending.stderr.contains("run owner-phase again after 20"),
            "the activation time must be printed: {}",
            pending.stderr
        );
        assert!(pending.stdout.is_empty(), "no identity while pending");
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let activated = operator.owner_phase(&plan, &enroll).await;
        assert_exit(
            &activated,
            SUCCESS,
            "owner-phase after the activation delay",
        );
        provisioned.admin = admin_pool(&provisioned).await;
        let state: String = sqlx::query_scalar(&format!(
            "SELECT state FROM {SCHEMA}.clustering_keypool WHERE singleton"
        ))
        .fetch_one(&provisioned.admin)
        .await
        .expect("read keypool state");
        assert_eq!(state, "active");
        provisioned.cleanup().await;

        // A database prepared without clustering, on which an unattested
        // server then created its keypair-slot table.
        let mut provisioned = Provisioned::new(&url).await;
        let deployment = provisioned
            .plan
            .lineage
            .deployment_uuid
            .expect("fixture deployment identity")
            .to_string();
        let single_node = Operator::new(
            &url,
            &provisioned.database,
            &provisioned.roles,
            SCHEMA,
            deployment.clone(),
        );
        assert_exit(
            &single_node.owner_phase(&[], &enroll).await,
            SUCCESS,
            "owner-phase without clustering",
        );
        let mut owner_config =
            DatabaseConfig::new(DatabaseDriver::Postgres, provisioned.owner_url.clone());
        owner_config.pool_size = 1;
        let owner = Database::from_config("unattested lease owner", &owner_config)
            .await
            .expect("owner database");
        drop(
            PostgresKeypairSlotLease::initialize_owner(owner.clone())
                .await
                .expect("create the unenrolled lease table"),
        );
        drop(owner);
        let operator = clustered_operator(
            &url,
            &provisioned.database,
            &provisioned.roles,
            SCHEMA,
            deployment,
            &keypairs,
        );
        provisioned.admin.close().await;
        let legacy = operator
            .owner_phase(
                &[
                    "--keypool-generation",
                    generation.as_str(),
                    "--legacy-keypair-slots",
                    "refuse",
                ],
                &[],
            )
            .await;
        assert_exit(&legacy, REFUSED, "owner-phase on an unenrolled lease table");
        assert!(
            legacy
                .stderr
                .contains("keypair-slot table created by an unattested runtime exists"),
            "{}",
            legacy.stderr
        );
        provisioned.admin = admin_pool(&provisioned).await;
        assert!(!relation_exists(&provisioned.admin, "clustering_keypool").await);
        provisioned.cleanup().await;
    }

    const APP_SCHEMA: &str = "public";
    const LEASE_TTL: Duration = Duration::from_millis(1500);

    /// Clustering settings both the owner-as-runtime server and the
    /// restricted server run with: one node, short leases.
    fn cluster_env(keypairs: &KeypairPool) -> Vec<(&'static str, String)> {
        let swarm_port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
            listener.local_addr().expect("local address").port()
        };
        vec![
            ("WADDLE_CLUSTERING_ENABLED", "true".to_owned()),
            (
                "WADDLE_CLUSTERING_LISTEN_ADDRS",
                format!("/ip4/127.0.0.1/tcp/{swarm_port}"),
            ),
            ("WADDLE_CLUSTERING_KEYPAIR_POOL", keypairs.seeds.join(",")),
            ("WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS", "500".to_owned()),
            (
                "WADDLE_CLUSTERING_LEASE_TTL_MS",
                LEASE_TTL.as_millis().to_string(),
            ),
            (
                "WADDLE_CLUSTERING_NODE_LEASE_HEARTBEAT_INTERVAL_MS",
                "300".to_owned(),
            ),
            ("WADDLE_CLUSTERING_NODE_LEASE_TTL_MS", "1200".to_owned()),
            (
                "WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS",
                "300".to_owned(),
            ),
            ("WADDLE_CLUSTERING_ISOLATION_INTERVALS", "2".to_owned()),
            (
                "WADDLE_CLUSTERING_REREGISTER_BACKOFF_BASE_MS",
                "200".to_owned(),
            ),
            (
                "WADDLE_CLUSTERING_REREGISTER_BACKOFF_MAX_MS",
                "1000".to_owned(),
            ),
            (
                "WADDLE_CLUSTERING_RESUME_HANDSHAKE_TIMEOUT_MS",
                "1000".to_owned(),
            ),
            (
                "WADDLE_CLUSTERING_ORPHAN_REAPER_INTERVAL_MS",
                "500".to_owned(),
            ),
            ("WADDLE_SERVER_OWNER_LOCALPARTS", "admin,alice".to_owned()),
        ]
    }

    async fn connect(server: &TestServer, user: &str, password: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, "cutover")
            .await
            .unwrap_or_else(|error| panic!("{user} authenticates and binds: {error}"))
    }

    /// Enter `room` as its owner, retrying while the room's claim moves to
    /// this node.
    async fn enter_room(admin: &mut WsXmppClient, room: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            send_element(admin, muc_presence(&format!("{room}/owner"), false)).await;
            let arrival = tokio::time::Instant::now() + Duration::from_secs(3);
            while let Ok(frame) = admin
                .recv_timeout(arrival.saturating_duration_since(tokio::time::Instant::now()))
                .await
            {
                if frame.parse::<Element>().is_ok_and(|stanza| {
                    stanza.name() == "message"
                        && has_descendant(&stanza, "subject", xmpp_parsers::ns::JABBER_CLIENT)
                }) {
                    return;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the room {room} could not be entered"
            );
        }
    }

    /// One digest per table over every row, as the administrator reads it.
    async fn row_digests(admin: &PgPool, tables: &[&str]) -> Vec<String> {
        let mut digests = Vec::new();
        for table in tables {
            let (rows, digest): (i64, Option<String>) = sqlx::query_as(&format!(
                "SELECT pg_catalog.count(*), \
                        pg_catalog.md5(pg_catalog.string_agg(t::text, ',' ORDER BY t::text)) \
                   FROM {APP_SCHEMA}.{table} t"
            ))
            .fetch_one(admin)
            .await
            .unwrap_or_else(|error| panic!("digest {table}: {error}"));
            assert!(rows > 0, "{table} holds no row to carry across the cutover");
            digests.push(format!("{table} {rows} {}", digest.unwrap_or_default()));
        }
        digests
    }

    /// A database shaped like one the owner-as-runtime server built: a
    /// single login role that owns the database and every relation in
    /// `public`, default PUBLIC grants, and a clustered server's unenrolled
    /// keypair-slot table. The documented provisioning procedure, then the
    /// subcommands, turn it into one the restricted server serves on, with
    /// the user, roster, archive and room it held.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn production_shaped_public_schema_is_provisioned_and_served() {
        let Some(url) = require_test_postgres_url() else {
            return;
        };
        let keypairs = keypair_pool(2);
        let cluster = PgPool::connect(&url).await.expect("cluster administrator");
        let suffix = Uuid::new_v4().simple().to_string();
        let database = format!("production_db_{suffix}");
        let roles = Roles {
            database_owner: format!("production_dbowner_{suffix}"),
            schema_owner: format!("production_owner_{suffix}"),
            migrator: format!("production_migrator_{suffix}"),
            runtime: format!("production_runtime_{suffix}"),
            // The login production serves under today.
            old_runtime: format!("production_app_{suffix}"),
            installer: format!("production_installer_{suffix}"),
            definer: format!("production_definer_{suffix}"),
        };
        execute(
            &cluster,
            &format!("CREATE ROLE {} LOGIN", roles.old_runtime),
        )
        .await;
        execute(
            &cluster,
            &format!("CREATE DATABASE {database} OWNER {}", roles.old_runtime),
        )
        .await;
        let admin_options = PgConnectOptions::from_str(&url)
            .expect("PostgreSQL test URL")
            .database(&database);
        let deployment = Uuid::new_v4().to_string();
        let app_url = login_url(&url, &database, &roles.old_runtime, "app-password");

        // The deployment pipeline enrolls the allowlist before the first
        // clustered node starts.
        let mut app_config = DatabaseConfig::new(DatabaseDriver::Postgres, app_url.clone());
        app_config.pool_size = 1;
        let app = Database::from_config("production application login", &app_config)
            .await
            .expect("application login connects");
        PostgresAllowlistStore::new(app.clone())
            .ensure_schema()
            .await
            .expect("create the peer allowlist");
        for peer in &keypairs.peer_ids {
            app.guard()
                .await
                .expect("application connection")
                .execute(
                    "INSERT INTO clustering_peer_allowlist (peer_id) VALUES (?)",
                    waddle_server::db_params![peer.to_string()],
                )
                .await
                .expect("enroll a peer");
        }
        drop(app);

        // The server as production runs it: clustered, under the login that
        // owns the schema, migrating and provisioning `public` itself.
        let mut env = cluster_env(&keypairs);
        env.extend([
            ("WADDLE_DB_DRIVER", "postgres".to_owned()),
            ("WADDLE_DATABASE_URL", app_url),
            ("WADDLE_DEPLOYMENT_UUID", deployment.clone()),
            ("WADDLE_DB_LINEAGE_ACTION", "enroll".to_owned()),
        ]);
        let mut server = super::start_server(env).await;
        let mut alice = connect(&server, "alice", "alice-password").await;
        let mut bob = connect(&server, "bob", "bob-password").await;
        let mut admin = connect(&server, "admin", server.fixed_account_password()).await;
        exercise_roster(&mut alice).await;
        send_element(
            &mut bob,
            Element::builder("presence", xmpp_parsers::ns::JABBER_CLIENT).build(),
        )
        .await;
        let direct = format!("before-cutover-direct-{suffix}");
        send_element(
            &mut alice,
            chat_message("bob@localhost", MessageType::Chat, "direct", &direct),
        )
        .await;
        collect_until(&mut bob, "direct message", |stanzas| {
            stanzas.iter().any(|stanza| contains_text(stanza, &direct))
        })
        .await;
        let room = format!("production-{suffix}@muc.localhost");
        enter_room(&mut admin, &room).await;
        let in_room = format!("before-cutover-room-{suffix}");
        send_element(
            &mut admin,
            chat_message(&room, MessageType::Groupchat, "room", &in_room),
        )
        .await;
        collect_until(&mut admin, "room message", |stanzas| {
            stanzas.iter().any(|stanza| contains_text(stanza, &in_room))
        })
        .await;
        for client in [alice, bob, admin] {
            client.close().await.expect("client closes");
        }

        // Cutover steps 1 to 4: the pods stop and no backend of the old
        // login remains.
        server.send_sigterm();
        assert!(
            server.wait_for_exit(Duration::from_secs(60)).await,
            "the owner-as-runtime server did not exit"
        );
        drop(server);
        let admin_pool = PgPool::connect_with(admin_options.clone())
            .await
            .expect("database administrator");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let sessions: i64 = sqlx::query_scalar(
                "SELECT pg_catalog.count(*) FROM pg_catalog.pg_stat_activity \
                  WHERE datname = pg_catalog.current_database() AND usename = $1",
            )
            .bind(&roles.old_runtime)
            .fetch_one(&admin_pool)
            .await
            .expect("count the old login's sessions");
            if sessions == 0 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the old login's sessions did not end"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let public_grants: bool = sqlx::query_scalar(
            "SELECT pg_catalog.has_schema_privilege('public', 'public', 'USAGE') \
                AND (SELECT pg_catalog.pg_get_userbyid(c.relowner) = $1 \
                       FROM pg_catalog.pg_class c \
                      WHERE c.oid = 'public.users'::pg_catalog.regclass)",
        )
        .bind(&roles.old_runtime)
        .fetch_one(&admin_pool)
        .await
        .expect("read the production shape");
        assert!(
            public_grants,
            "the fixture must have production's shape: PUBLIC on `public`, tables owned by the login"
        );
        const CARRIED: [&str; 4] = [
            "native_users",
            "roster_items",
            "mam_messages",
            "clustering_muc_rooms",
        ];
        let before = row_digests(&admin_pool, &CARRIED).await;
        admin_pool.close().await;

        // The documented provisioning procedure, exactly as written. A
        // second run leaves every role, membership, owner and ACL as the
        // first left it.
        let provisioned_roles = roles.all().map(String::as_str);
        provision(&admin_options, APP_SCHEMA, &roles, &[])
            .await
            .expect("provisioning");
        let provisioned = provisioning_snapshot(&admin_options, &provisioned_roles).await;
        provision(&admin_options, APP_SCHEMA, &roles, &[])
            .await
            .expect("repeated provisioning");
        assert_eq!(
            provisioning_snapshot(&admin_options, &provisioned_roles).await,
            provisioned,
            "a repeated provisioning run changed the catalog"
        );

        // The old login keeps its role but owns nothing here and cannot
        // connect; the application schema belongs to the NOLOGIN owner.
        let admin_pool = PgPool::connect_with(admin_options.clone())
            .await
            .expect("database administrator");
        let (owned, connects, schema_owner): (i64, bool, String) = sqlx::query_as(
            "SELECT (SELECT pg_catalog.count(*) FROM pg_catalog.pg_class c \
                      WHERE pg_catalog.pg_get_userbyid(c.relowner) = $1), \
                    pg_catalog.has_database_privilege($1, pg_catalog.current_database(), 'CONNECT'), \
                    (SELECT pg_catalog.pg_get_userbyid(n.nspowner) \
                       FROM pg_catalog.pg_namespace n WHERE n.nspname = 'public')",
        )
        .bind(&roles.old_runtime)
        .fetch_one(&admin_pool)
        .await
        .expect("read the provisioned shape");
        assert_eq!(
            (owned, connects, schema_owner.as_str()),
            (0, false, roles.schema_owner.as_str()),
            "the old login must own nothing and the NOLOGIN owner must own `public`"
        );
        admin_pool.close().await;

        // Step 5: the owner phase, with the unenrolled lease table retired
        // under the lease lifetime the old server ran with.
        let mut operator = clustered_operator(
            &url,
            &database,
            &roles,
            APP_SCHEMA,
            deployment.clone(),
            &keypairs,
        );
        operator.env.extend([
            (
                "WADDLE_CLUSTERING_LEASE_TTL_MS",
                LEASE_TTL.as_millis().to_string(),
            ),
            ("WADDLE_CLUSTERING_HEARTBEAT_INTERVAL_MS", "500".to_owned()),
        ]);
        let files = tempfile::tempdir().expect("operator files");
        let identity_path = files.path().join("identity.json");
        let generation = Uuid::new_v4().to_string();
        let plan = [
            "--keypool-generation",
            generation.as_str(),
            "--legacy-keypair-slots",
            "retire",
            "--legacy-ttl",
            "1500ms",
            "--identity-output",
            identity_path.to_str().expect("identity path"),
        ];
        tokio::time::sleep(LEASE_TTL + Duration::from_millis(300)).await;
        let pending = operator.owner_phase(&plan, &[]).await;
        assert_exit(&pending, PENDING, "owner-phase retiring the lease table");
        tokio::time::sleep(LEASE_TTL + Duration::from_millis(500)).await;
        let prepared = operator.owner_phase(&plan, &[]).await;
        assert_exit(&prepared, SUCCESS, "owner-phase after the activation delay");

        let admin_pool = PgPool::connect_with(admin_options.clone())
            .await
            .expect("database administrator");
        assert_eq!(
            row_digests(&admin_pool, &CARRIED).await,
            before,
            "provisioning and the owner phase must keep every carried row"
        );

        let operation_id = Uuid::new_v4();
        assert_exit(
            &operator
                .install_attestor(&identity_path, operation_id)
                .await,
            SUCCESS,
            "install-attestor",
        );
        let endpoint = SocketEndpoint::for_url(&url).await;
        let runtime_url = runtime_url(&endpoint, &database, &roles.runtime, APP_SCHEMA);
        let cutover_path = files.path().join("cutover.json");
        std::fs::write(
            &cutover_path,
            cutover_record(&admin_pool, &roles.old_runtime, &runtime_url).await,
        )
        .expect("cutover record");
        let operation_arg = operation_id.to_string();
        let emitted = operator
            .emit_handoff(
                &[
                    "--release-git-sha",
                    RELEASE_GIT_SHA,
                    "--installation-operation-id",
                    &operation_arg,
                    "--cutover",
                    cutover_path.to_str().expect("cutover path"),
                ],
                &[("WADDLE_RUNTIME_DATABASE_URL", runtime_url.as_str())],
            )
            .await;
        assert_exit(&emitted, SUCCESS, "emit-handoff with the pinned preflight");

        // Step 6: the new version, under the runtime login.
        let mut env = cluster_env(&keypairs);
        env.extend([
            ("WADDLE_DB_DRIVER", "postgres".to_owned()),
            ("WADDLE_DATABASE_URL", runtime_url),
            ("WADDLE_DEPLOYMENT_UUID", deployment),
            ("WADDLE_GIT_SHA", RELEASE_GIT_SHA.to_owned()),
            ("WADDLE_DB_RUNTIME_ATTESTATION", emitted.stdout),
            ("WADDLE_DB_MIGRATOR_LOGIN", roles.migrator.clone()),
            ("WADDLE_XMPP_MAM_SCHEMA_OWNER", roles.schema_owner.clone()),
            ("WADDLE_XMPP_MAM_CLAIM_SCHEMA", APP_SCHEMA.to_owned()),
            ("WADDLE_SESSION_KEY", SESSION_KEY.to_owned()),
        ]);
        let server = super::start_server(env).await;
        assert_serves(&server).await;

        let mut alice = connect(&server, "alice", "alice-password").await;
        let roster = iq_result(
            &mut alice,
            iq_request(
                false,
                "roster-after",
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
            "the roster item must survive the cutover: {roster:?}"
        );
        let archive = mam_query(&mut alice, "archive-after", None).await;
        assert!(
            archive.iter().any(|stanza| contains_text(stanza, &direct)),
            "the archived direct message must survive the cutover: {archive:?}"
        );
        let mut admin = connect(&server, "admin", server.fixed_account_password()).await;
        enter_room(&mut admin, &room).await;
        let room_archive = mam_query(&mut admin, "room-after", Some(&room)).await;
        assert!(
            room_archive
                .iter()
                .any(|stanza| contains_text(stanza, &in_room)),
            "the room and its archive must survive the cutover: {room_archive:?}"
        );
        alice.close().await.expect("alice closes");
        admin.close().await.expect("admin closes");
        let refused: Vec<_> = endpoint
            .take_wire()
            .errors
            .into_iter()
            .filter(|(sqlstate, _)| sqlstate == "42501")
            .collect();
        assert!(
            refused.is_empty(),
            "the runtime login was refused a privilege on `public`: {refused:#?}"
        );

        drop(server);
        drop_fixture(cluster, admin_pool, &database, &roles).await;
        drop(endpoint);
    }
}
