//! The TLS terminator is a test fixture, not a suitable production endpoint.
//! Each accepted client maps to one PostgreSQL session.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::{Connection, PgConnection};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use waddle_server::db::lineage::{DeploymentUuid, LineageUuid};
use waddle_server::db::physical_attestation::{
    install, observe_installation, InstallationObservation, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};

use super::{test_url, Fixture, CATALOG_GRANT_LOCK};

struct TlsFixture {
    _directory: tempfile::TempDir,
    task: JoinHandle<()>,
    url: String,
}

impl Drop for TlsFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TlsFixture {
    async fn new(fixture: &Fixture) -> Self {
        let directory = tempfile::tempdir().expect("TLS CA directory");
        let ca = directory.path().join("ca.pem");
        let mut ca_params = CertificateParams::new(Vec::new()).expect("CA parameters");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages.push(KeyUsagePurpose::KeyCertSign);
        let ca_key = KeyPair::generate().expect("CA key");
        let ca_cert = ca_params.self_signed(&ca_key).expect("CA certificate");
        std::fs::write(&ca, ca_cert.pem()).expect("write test CA");
        let params = CertificateParams::new(vec!["localhost".to_owned()]).expect("leaf parameters");
        let leaf_key = KeyPair::generate().expect("leaf key");
        let certificate = params
            .signed_by(&leaf_key, &ca_cert, &ca_key)
            .expect("leaf certificate");
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_no_client_auth()
        .with_single_cert(vec![certificate.der().clone()], key)
        .expect("test TLS certificate");
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let upstream: PgConnectOptions = fixture.base_url.parse().expect("upstream options");
        let upstream_host = upstream.get_host().to_owned();
        let upstream_port = upstream.get_port();
        #[cfg(unix)]
        let upstream_socket = upstream
            .get_socket()
            .cloned()
            .or_else(|| {
                upstream_host
                    .starts_with('/')
                    .then(|| upstream_host.clone().into())
            })
            .map(|directory| directory.join(format!(".s.PGSQL.{upstream_port}")));
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("TLS listener");
        let port = listener.local_addr().expect("TLS listener address").port();
        let task = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let upstream_host = upstream_host.clone();
                #[cfg(unix)]
                let upstream_socket = upstream_socket.clone();
                tokio::spawn(async move {
                    let mut request = [0u8; 8];
                    if client.read_exact(&mut request).await.is_err()
                        || request != [0, 0, 0, 8, 4, 210, 22, 47]
                        || client.write_all(b"S").await.is_err()
                    {
                        return;
                    }
                    let Ok(mut client) = acceptor.accept(client).await else {
                        return;
                    };
                    #[cfg(unix)]
                    if let Some(socket) = upstream_socket {
                        if let Ok(mut upstream) = tokio::net::UnixStream::connect(socket).await {
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                        }
                        return;
                    }
                    let Ok(mut upstream) =
                        TcpStream::connect((upstream_host.as_str(), upstream_port)).await
                    else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
        let mut url = url::Url::parse(&format!(
            "postgresql://{}@localhost:{port}/{}",
            fixture.installer, fixture.database_name
        ))
        .expect("test URL");
        url.set_password(Some("observer-test-password"))
            .expect("test password");
        url.query_pairs_mut()
            .append_pair("sslmode", "verify-full")
            .append_pair("sslrootcert", ca.to_str().expect("CA path"));
        Self {
            _directory: directory,
            task,
            url: url.to_string(),
        }
    }

    fn options(&self) -> PgConnectOptions {
        self.url.parse().expect("verified TLS options")
    }
}

async fn configure_observer(fixture: &Fixture) {
    for (setting, value) in [
        ("lock_timeout", "200ms"),
        ("statement_timeout", "2s"),
        ("idle_in_transaction_session_timeout", "3s"),
        ("idle_session_timeout", "3s"),
        ("transaction_timeout", "5s"),
    ] {
        sqlx::query(&format!(
            "ALTER ROLE {} SET {setting} = '{value}'",
            fixture.installer
        ))
        .execute(&fixture.cluster_admin)
        .await
        .expect("set tighter inherited bound");
    }
    let hostile = format!("hostile_{}", fixture.installer);
    sqlx::query(&format!(
        "CREATE SCHEMA {hostile} AUTHORIZATION {}",
        fixture.installer
    ))
    .execute(&fixture.admin)
    .await
    .expect("hostile path schema");
    sqlx::query(&format!("CREATE FUNCTION {hostile}.pg_is_in_recovery() RETURNS boolean LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'hostile search path called'; END $$"))
        .execute(&fixture.admin).await.expect("hostile function");
    sqlx::query(&format!(
        "ALTER ROLE {} SET search_path = {hostile}, pg_catalog",
        fixture.installer
    ))
    .execute(&fixture.cluster_admin)
    .await
    .expect("hostile inherited path");
}

async fn catalog(fixture: &Fixture) -> String {
    let sql = format!(
        r#"SELECT pg_catalog.jsonb_build_object(
      'namespace', (SELECT pg_catalog.to_jsonb(n) FROM pg_catalog.pg_namespace n WHERE n.nspname = $1),
      'functions', (SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(p) ORDER BY p.oid) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1),
      'roles', (SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(r) ORDER BY r.oid) FROM pg_catalog.pg_roles r WHERE r.rolname IN ($2, $3, $4)),
      'role_settings', (SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(s) ORDER BY s.setrole, s.setdatabase) FROM pg_catalog.pg_db_role_setting s WHERE s.setrole IN (SELECT r.oid FROM pg_catalog.pg_roles r WHERE r.rolname IN ($2, $3, $4))),
      'memberships', (SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(m) ORDER BY m.roleid, m.member, m.grantor) FROM pg_catalog.pg_auth_members m WHERE m.roleid IN (SELECT r.oid FROM pg_catalog.pg_roles r WHERE r.rolname IN ($2, $3, $4)) OR m.member IN (SELECT r.oid FROM pg_catalog.pg_roles r WHERE r.rolname IN ($2, $3, $4))),
      'lineage_relation', (SELECT pg_catalog.to_jsonb(c) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $5 AND c.relname = '_lineage'),
      'lineage_columns', (SELECT pg_catalog.jsonb_agg(pg_catalog.to_jsonb(a) ORDER BY a.attnum) FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid = a.attrelid JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $5 AND c.relname = '_lineage'),
      'control_function', (SELECT pg_catalog.to_jsonb(p) FROM pg_catalog.pg_proc p WHERE p.oid = 'pg_catalog.pg_control_system()'::pg_catalog.regprocedure),
      'lineage', (SELECT pg_catalog.to_jsonb(l) FROM {}._lineage l)
    )::pg_catalog.text"#,
        fixture.target
    );
    sqlx::query_scalar(&sql)
        .bind(&fixture.trusted)
        .bind(&fixture.installer)
        .bind(&fixture.runtime)
        .bind(&fixture.definer)
        .bind(&fixture.target)
        .fetch_one(&fixture.admin)
        .await
        .expect("relevant catalog snapshot")
}

fn cli(fixture: &Fixture, tls: &TlsFixture) -> tokio::process::Command {
    let exe = std::env::var_os("NEXTEST_BIN_EXE_waddle_attestation_observe")
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_waddle-attestation-observe"))
        .expect("observer executable path");
    let pg = fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("fixture identity");
    let mut command = tokio::process::Command::new(exe);
    command
        .kill_on_drop(true)
        .env_remove("PGOPTIONS")
        .env_remove("PGAPPNAME")
        .env("WADDLE_ATTESTATION_OBSERVE_DATABASE_URL", &tls.url)
        .args([
            "--direct-endpoint",
            "--operation-id",
            &fixture.operation_id.to_string(),
            "--expected-deployment-uuid",
            &fixture.lineage.deployment_uuid.to_string(),
            "--expected-lineage-uuid",
            &fixture.lineage.lineage_uuid.to_string(),
            "--expected-system-identifier",
            &pg.system_identifier.to_string(),
            "--expected-database-oid",
            &pg.database.oid.to_string(),
            "--expected-database-name",
            &pg.database.name.0,
            "--expected-schema-oid",
            &pg.schema.oid.to_string(),
            "--expected-schema-name",
            &pg.schema.name.0,
            "--installer-login",
            &fixture.installer,
            "--runtime-login",
            &fixture.runtime,
            "--definer-role",
            &fixture.definer,
            "--trusted-schema",
            &fixture.trusted,
        ]);
    command
}

#[tokio::test]
async fn operator_observer_reports_absence_identity_and_catalog_drift_without_mutation() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    configure_observer(&fixture).await;
    let tls = TlsFixture::new(&fixture).await;
    let before = catalog(&fixture).await;
    assert_eq!(
        observe_installation(&tls.options(), &fixture.config(), fixture.operation_id).await,
        InstallationObservation::Indeterminate {
            reason: "installation_not_visible"
        }
    );
    assert_eq!(catalog(&fixture).await, before);
    install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install");
    let before = catalog(&fixture).await;
    let output = cli(&fixture, &tls).output().await.expect("observer CLI");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON observation");
    assert_eq!(value["status"], "observed");
    assert_eq!(value["writer_activation_authority"], false);
    assert_eq!(
        value["scope"]["kind"],
        "one_direct_session_catalog_and_lineage_snapshot"
    );
    assert_eq!(
        value["scope"]["operation_id"],
        fixture.operation_id.to_string()
    );
    assert_eq!(
        value["scope"]["expected"]["deployment_uuid"],
        fixture.lineage.deployment_uuid.to_string()
    );
    assert_eq!(
        value["scope"]["expected"]["lineage_uuid"],
        fixture.lineage.lineage_uuid.to_string()
    );
    let identity = fixture
        .lineage
        .postgres_identity
        .as_ref()
        .expect("fixture identity");
    assert_eq!(
        value["scope"]["expected"]["system_identifier"],
        identity.system_identifier.to_string()
    );
    assert_eq!(
        value["scope"]["expected"]["database_oid"],
        identity.database.oid
    );
    assert_eq!(
        value["scope"]["expected"]["database_name"],
        identity.database.name.0
    );
    assert_eq!(
        value["scope"]["expected"]["schema_oid"],
        identity.schema.oid
    );
    assert_eq!(
        value["scope"]["expected"]["schema_name"],
        identity.schema.name.0
    );
    assert_eq!(
        value["scope"]["expected"]["installer_login"],
        fixture.installer
    );
    assert_eq!(value["scope"]["expected"]["runtime_login"], fixture.runtime);
    assert_eq!(value["scope"]["expected"]["definer_role"], fixture.definer);
    assert_eq!(
        value["scope"]["expected"]["trusted_schema"],
        fixture.trusted
    );
    assert_eq!(value["scope"]["endpoint"]["host"], "localhost");
    assert_eq!(value["scope"]["endpoint"]["port"], tls.options().get_port());
    assert_eq!(
        value["scope"]["endpoint"]["database"],
        fixture.database_name
    );
    assert_eq!(value["scope"]["endpoint"]["tls_mode"], "verify-full");
    assert_eq!(
        value["scope"]["endpoint"]["direct_and_fenced"],
        "operator_assertion"
    );
    assert!(value["started_at"].as_str().is_some());
    assert!(value["finished_at"].as_str().is_some());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&tls.url));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("observer-test-password"));
    assert_eq!(catalog(&fixture).await, before);
    assert_eq!(
        observe_installation(&tls.options(), &fixture.config(), uuid::Uuid::new_v4()).await,
        InstallationObservation::Conflict {
            reason: "installation_catalog"
        }
    );
    let mut wrong = fixture.lineage.clone();
    wrong.deployment_uuid = DeploymentUuid(uuid::Uuid::new_v4());
    assert!(matches!(
        observe_installation(
            &tls.options(),
            &fixture.with_lineage(wrong),
            fixture.operation_id
        )
        .await,
        InstallationObservation::Conflict {
            reason: "deployment UUID"
        }
    ));
    let mut wrong_lineage = fixture.lineage.clone();
    wrong_lineage.lineage_uuid = LineageUuid(uuid::Uuid::new_v4());
    let mut wrong_system = fixture.lineage.clone();
    wrong_system
        .postgres_identity
        .as_mut()
        .expect("physical identity")
        .system_identifier
        .0 ^= 1;
    let mut wrong_database_oid = fixture.lineage.clone();
    wrong_database_oid
        .postgres_identity
        .as_mut()
        .expect("physical identity")
        .database
        .oid ^= 1;
    let mut wrong_schema_oid = fixture.lineage.clone();
    wrong_schema_oid
        .postgres_identity
        .as_mut()
        .expect("physical identity")
        .schema
        .oid ^= 1;
    for wrong in [
        wrong_lineage,
        wrong_system,
        wrong_database_oid,
        wrong_schema_oid,
    ] {
        assert!(matches!(
            observe_installation(
                &tls.options(),
                &fixture.with_lineage(wrong),
                fixture.operation_id
            )
            .await,
            InstallationObservation::Conflict { .. }
        ));
    }
    sqlx::query(&format!("ALTER ROLE {} CREATEDB", fixture.runtime))
        .execute(&fixture.cluster_admin)
        .await
        .expect("drift runtime role");
    let drift = catalog(&fixture).await;
    assert_eq!(
        observe_installation(&tls.options(), &fixture.config(), fixture.operation_id).await,
        InstallationObservation::Conflict {
            reason: "runtime privileges"
        }
    );
    assert_eq!(catalog(&fixture).await, drift);
    sqlx::query(&format!("ALTER ROLE {} NOCREATEDB", fixture.runtime))
        .execute(&fixture.cluster_admin)
        .await
        .expect("restore runtime role");

    sqlx::query(&format!(
        "UPDATE {}._lineage SET lineage_uuid = $1",
        fixture.target
    ))
    .bind(uuid::Uuid::new_v4().to_string())
    .execute(&fixture.admin)
    .await
    .expect("drift lineage");
    let drift = catalog(&fixture).await;
    assert_eq!(
        observe_installation(&tls.options(), &fixture.config(), fixture.operation_id).await,
        InstallationObservation::Conflict {
            reason: "lineage UUID"
        }
    );
    assert_eq!(catalog(&fixture).await, drift);
    sqlx::query(&format!(
        "UPDATE {}._lineage SET lineage_uuid = $1",
        fixture.target
    ))
    .bind(fixture.lineage.lineage_uuid.to_string())
    .execute(&fixture.admin)
    .await
    .expect("restore lineage");

    // PostgreSQL text fields must not compare equal after truncating a valid
    // numeric prefix. Leading zeroes let a 128-character prefix parse to the
    // expected number even though the stored value has an extra suffix.
    for (column, expected) in [
        (
            "pg_system_identifier",
            identity.system_identifier.to_string(),
        ),
        ("pg_database_oid", identity.database.oid.to_string()),
        ("pg_schema_oid", identity.schema.oid.to_string()),
    ] {
        let prefix = format!("{}{expected}", "0".repeat(128 - expected.len()));
        assert_eq!(
            prefix.parse::<u64>().expect("numeric prefix"),
            expected.parse::<u64>().expect("expected number")
        );
        sqlx::query(&format!(
            "UPDATE {}._lineage SET {column} = $1",
            fixture.target
        ))
        .bind(format!("{prefix}x"))
        .execute(&fixture.admin)
        .await
        .expect("install oversized value");
        let oversized = catalog(&fixture).await;
        assert!(matches!(
            observe_installation(&tls.options(), &fixture.config(), fixture.operation_id).await,
            InstallationObservation::Indeterminate { .. }
        ));
        assert_eq!(catalog(&fixture).await, oversized);
        sqlx::query(&format!(
            "UPDATE {}._lineage SET {column} = $1",
            fixture.target
        ))
        .bind(expected)
        .execute(&fixture.admin)
        .await
        .expect("restore identity field");
    }

    sqlx::query(&format!("DELETE FROM {}._lineage", fixture.target))
        .execute(&fixture.admin)
        .await
        .expect("remove lineage row");
    let missing = catalog(&fixture).await;
    assert!(matches!(
        observe_installation(&tls.options(), &fixture.config(), fixture.operation_id).await,
        InstallationObservation::Indeterminate { .. }
    ));
    assert_eq!(catalog(&fixture).await, missing);
    sqlx::query(&format!("INSERT INTO {}._lineage (id, format, lineage_uuid, deployment_uuid, pg_system_identifier, pg_database_oid, pg_database_name, pg_schema_oid, pg_schema_name) VALUES (1, 1, $1, $2, $3, $4, $5, $6, $7)", fixture.target))
        .bind(fixture.lineage.lineage_uuid.to_string())
        .bind(fixture.lineage.deployment_uuid.to_string())
        .bind(identity.system_identifier.to_string())
        .bind(identity.database.oid.to_string())
        .bind(&identity.database.name.0)
        .bind(identity.schema.oid.to_string())
        .bind(&identity.schema.name.0)
        .execute(&fixture.admin).await.expect("restore lineage row");
    sqlx::query(&format!(
        "ALTER FUNCTION {}.read_physical_identity() SET statement_timeout = '1s'",
        fixture.trusted
    ))
    .execute(&fixture.admin)
    .await
    .expect("drift function definition");
    let drift = catalog(&fixture).await;
    assert_eq!(
        observe_installation(&tls.options(), &fixture.config(), fixture.operation_id).await,
        InstallationObservation::Conflict {
            reason: "installation_catalog"
        }
    );
    assert_eq!(catalog(&fixture).await, drift);
    assert_eq!(
        observe_installation(
            &tls.options().ssl_mode(PgSslMode::Disable),
            &fixture.config(),
            fixture.operation_id
        )
        .await,
        InstallationObservation::Indeterminate {
            reason: "connection_configuration"
        }
    );
    assert_eq!(
        observe_installation(
            &tls.options().host("/private/tmp"),
            &fixture.config(),
            fixture.operation_id
        )
        .await,
        InstallationObservation::Indeterminate {
            reason: "connection_configuration"
        }
    );
    assert!(matches!(
        observe_installation(
            &tls.options().ssl_root_cert("/dev/null"),
            &fixture.config(),
            fixture.operation_id
        )
        .await,
        InstallationObservation::Indeterminate { .. }
    ));
    assert_eq!(
        observe_installation(
            &tls.options().options([("statement_timeout", "30s")]),
            &fixture.config(),
            fixture.operation_id
        )
        .await,
        InstallationObservation::Indeterminate {
            reason: "connection_configuration"
        }
    );
    drop(tls);
    fixture.cleanup().await;
}

#[tokio::test]
async fn operator_observer_refuses_spoof_without_invocation() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    configure_observer(&fixture).await;
    let tls = TlsFixture::new(&fixture).await;
    sqlx::query(&format!("CREATE FUNCTION {}.read_physical_identity() RETURNS integer LANGUAGE plpgsql SECURITY DEFINER AS $$ BEGIN PERFORM pg_catalog.pg_sleep(5); RETURN 1; END $$", fixture.trusted))
        .execute(&fixture.admin).await.expect("spoof function");
    sqlx::query(&format!(
        "ALTER FUNCTION {}.read_physical_identity() OWNER TO {}",
        fixture.trusted, fixture.definer
    ))
    .execute(&fixture.admin)
    .await
    .expect("spoof owner");
    let before = catalog(&fixture).await;
    let start = Instant::now();
    assert_eq!(
        observe_installation(&tls.options(), &fixture.config(), fixture.operation_id).await,
        InstallationObservation::Conflict {
            reason: "installation_catalog"
        }
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "spoof function must not run"
    );
    assert_eq!(catalog(&fixture).await, before);
    drop(tls);
    fixture.cleanup().await;
}

#[tokio::test]
async fn operator_observer_waits_for_catalog_before_other_barriers() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install fixture attestor");
    configure_observer(&fixture).await;
    sqlx::query(&format!(
        "ALTER ROLE {} SET lock_timeout = '2s'",
        fixture.installer
    ))
    .execute(&fixture.cluster_admin)
    .await
    .expect("allow bounded observer retry");
    let tls = TlsFixture::new(&fixture).await;
    let mut blocker = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("catalog blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut blocker)
        .await
        .expect("hold catalog exclusive");
    let application = format!("observer_catalog_{}", uuid::Uuid::new_v4().simple());
    let options = tls.options().application_name(&application);
    let config = fixture.config();
    let operation = fixture.operation_id;
    let task =
        tokio::spawn(async move { observe_installation(&options, &config, operation).await });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let retrying: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity a \
                 WHERE a.application_name=$1 AND a.state='idle' AND a.backend_xmin IS NULL \
                 AND a.query LIKE '%pg_try_advisory_lock_shared%' AND NOT EXISTS \
                 (SELECT 1 FROM pg_catalog.pg_locks l WHERE l.pid=a.pid \
                  AND l.locktype='advisory' AND l.granted))",
            )
            .bind(&application)
            .fetch_one(&fixture.admin)
            .await
            .expect("inspect observer retry");
            if retrying {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("observer retries catalog without a snapshot or lower lock");
    assert!(!task.is_finished());
    let released: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(&mut blocker)
        .await
        .expect("release catalog exclusive");
    assert!(released);
    assert_eq!(
        task.await.expect("join observer"),
        InstallationObservation::Observed
    );
    drop(blocker);
    drop(tls);
    fixture.cleanup().await;
}

#[tokio::test]
async fn operator_observer_preserves_tighter_timeouts_and_bounds_cancellation() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    configure_observer(&fixture).await;
    let tls = TlsFixture::new(&fixture).await;
    let mut blocker = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("blocker");
    sqlx::query("BEGIN")
        .execute(&mut blocker)
        .await
        .expect("begin");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(6_841_445_497_037_937_993_i64)
        .execute(&mut blocker)
        .await
        .expect("block observer lock");
    let before = catalog(&fixture).await;

    // Positive control: SQLx's startup encoder forwards embedded NULs and the
    // backend accepts the injected option, lengthening the inherited 200ms
    // limit to 1900ms. The observer must reject this before connecting.
    let injection = "observer\0options\0-c lock_timeout=1900ms";
    let mut normal = PgConnection::connect_with(&tls.options())
        .await
        .expect("unmodified observer session");
    let normal_timeout: String = sqlx::query_scalar("SHOW lock_timeout")
        .fetch_one(&mut normal)
        .await
        .expect("read inherited timeout");
    assert_eq!(normal_timeout, "200ms");
    normal.close().await.expect("close unmodified session");
    let poisoned = tls.options().application_name(injection);
    assert!(poisoned.get_options().is_none());
    let mut raw = PgConnection::connect_with(&poisoned)
        .await
        .expect("raw SQLx startup injection control");
    let raw_timeout: String = sqlx::query_scalar("SHOW lock_timeout")
        .fetch_one(&mut raw)
        .await
        .expect("read injected timeout");
    assert_eq!(raw_timeout, "1900ms");
    raw.close().await.expect("close raw control");
    assert_eq!(
        observe_installation(&poisoned, &fixture.config(), fixture.operation_id).await,
        InstallationObservation::Indeterminate {
            reason: "connection_configuration"
        }
    );

    let mut injected_url = url::Url::parse(&tls.url).expect("TLS URL");
    injected_url
        .query_pairs_mut()
        .append_pair("application_name", injection);
    let url_options: PgConnectOptions = injected_url
        .as_str()
        .parse()
        .expect("encoded startup injection");
    assert_eq!(url_options.get_application_name(), Some(injection));
    assert!(url_options.get_options().is_none());
    assert_eq!(
        observe_installation(&url_options, &fixture.config(), fixture.operation_id).await,
        InstallationObservation::Indeterminate {
            reason: "connection_configuration"
        }
    );
    let output = cli(&fixture, &tls)
        .env(
            "WADDLE_ATTESTATION_OBSERVE_DATABASE_URL",
            injected_url.as_str(),
        )
        .output()
        .await
        .expect("injected CLI URL");
    assert_eq!(output.status.code(), Some(2));
    let value: Value = serde_json::from_slice(&output.stdout).expect("indeterminate JSON");
    assert_eq!(value["status"], "indeterminate");
    assert_eq!(value["reason"], "connection_configuration");

    let mut poisoned_lineage = fixture.lineage.clone();
    let poisoned_name = format!("{}\0options\0-c lock_timeout=1900ms", fixture.database_name);
    poisoned_lineage
        .postgres_identity
        .as_mut()
        .expect("physical identity")
        .database
        .name
        .0 = poisoned_name.clone();
    assert_eq!(
        observe_installation(
            &tls.options().database(&poisoned_name),
            &fixture.with_lineage(poisoned_lineage),
            fixture.operation_id
        )
        .await,
        InstallationObservation::Indeterminate {
            reason: "connection_configuration"
        }
    );
    assert_eq!(
        observe_installation(
            &tls.options().host("localhost\0injected"),
            &fixture.config(),
            fixture.operation_id
        )
        .await,
        InstallationObservation::Indeterminate {
            reason: "connection_configuration"
        }
    );
    assert_eq!(catalog(&fixture).await, before);
    let start = Instant::now();
    assert!(matches!(
        observe_installation(&tls.options(), &fixture.config(), fixture.operation_id).await,
        InstallationObservation::Indeterminate { .. }
    ));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "inherited 200ms lock timeout was relaxed"
    );
    assert_eq!(catalog(&fixture).await, before);
    sqlx::query(&format!(
        "ALTER ROLE {} SET lock_timeout = '1s'",
        fixture.installer
    ))
    .execute(&fixture.cluster_admin)
    .await
    .expect("allow time to observe cancellation");
    let before_cancel = catalog(&fixture).await;
    let application = format!("observer_cancel_{}", uuid::Uuid::new_v4().simple());
    let options = tls.options().application_name(&application);
    let config = fixture.config();
    let operation = fixture.operation_id;
    let task =
        tokio::spawn(async move { observe_installation(&options, &config, operation).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let retrying: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE application_name = $1 AND state = 'idle' AND query LIKE '%pg_advisory_unlock_shared%')")
                .bind(&application).fetch_one(&fixture.admin).await.expect("observe lock retry");
            if retrying { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("observer released the catalog fence before retry");
    task.abort();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let active: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE usename = $1",
            )
            .bind(&fixture.installer)
            .fetch_one(&fixture.admin)
            .await
            .expect("observer backends");
            if active == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("cancelled backend terminates");
    assert_eq!(catalog(&fixture).await, before_cancel);
    sqlx::query("ROLLBACK")
        .execute(&mut blocker)
        .await
        .expect("release blocker");
    sqlx::query(&format!(
        "ALTER ROLE {} SET transaction_timeout = '0'",
        fixture.installer
    ))
    .execute(&fixture.cluster_admin)
    .await
    .expect("unbounded transaction setting");
    assert_eq!(
        observe_installation(&tls.options(), &fixture.config(), fixture.operation_id).await,
        InstallationObservation::Indeterminate {
            reason: "observation_failed"
        }
    );
    drop(blocker);
    drop(tls);
    fixture.cleanup().await;
}
