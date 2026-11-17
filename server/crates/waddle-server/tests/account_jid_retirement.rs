//! Durable non-reuse of retired account JIDs on a Waddle home (ADR-0020 D8).
//!
//! Retiring an account records its JID in a ledger and removes the account.
//! No provisioning path issues that JID again: native registration refuses
//! it, provisioning at an identity-provider login passes it over like a name
//! another account holds (also when the same provider subject returns),
//! session creation creates neither an account row nor a session for it, and
//! the fixed-account bootstrap does not seed it. Retirement and provisioning
//! exclude each other, in both orders; a retirement that cannot lock the
//! account tables in time fails and changes nothing. Retirement also
//! removes what remote senders left for the account: recorded
//! subscription requests, remote PubSub subscription records and offline
//! admissions (gate G18), and the account's offline store under every
//! spelling of its JID.
//!
//! Every case runs on SQLite; the cases that take a store run on PostgreSQL
//! as well, on a database of their own, and the exclusion cases need
//! PostgreSQL. A lane that sets `WADDLE_TEST_REQUIRE_POSTGRES=1` fails them
//! without it. The operator subcommand runs as the server binary.
#![cfg(feature = "test-support")]

// The shared ingress fixture serves several suites; this one uses its
// isolated, migrated database only.
#[allow(dead_code)]
mod ingress_support;

use std::time::{Duration, Instant};

use ingress_support::IngressFixture;
use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use serde_json::json;
use waddle_server::auth::identity::IdentityService;
use waddle_server::auth::retirement::{jid_is_retired, retire_account, RetireAccountError};
use waddle_server::auth::{
    AuthError, AuthProviderConfig, AuthProviderKind, AuthProviderTokenEndpointAuthMethod,
    IdentityClaims, NativeUserStore, RegisterRequest, Session, SessionManager,
};
use waddle_server::db::actor::{DbActor, DbQueryOne};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};
use waddle_server::server::{seed_fixed_test_account, FixedTestAccountConfig};

const DOMAIN: &str = "example.com";

/// A migrated account store on one engine.
struct Store {
    engine: &'static str,
    db: Database,
    actor: ActorRef<DbActor>,
    postgres: Option<IngressFixture>,
}

impl Store {
    async fn sqlite(name: &str) -> Self {
        let db = Database::in_memory(name).await.expect("in-memory database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrations");
        Self {
            engine: "sqlite",
            actor: DbActor::spawn(DbActor::new(db.clone())),
            db,
            postgres: None,
        }
    }

    /// `None` when the lane provides no PostgreSQL and does not require it.
    async fn postgres(label: &str) -> Option<Self> {
        let fixture = IngressFixture::postgres(label).await?;
        let db = fixture.db.clone();
        Some(Self {
            engine: "postgres",
            actor: DbActor::spawn(DbActor::new(db.clone())),
            db,
            postgres: Some(fixture),
        })
    }

    /// The stores a case runs on: SQLite, and PostgreSQL when the lane has
    /// it.
    async fn each(label: &str) -> Vec<Self> {
        let mut stores = vec![Self::sqlite(label).await];
        stores.extend(Self::postgres(label).await);
        stores
    }

    fn native(&self) -> NativeUserStore {
        NativeUserStore::new(self.actor.clone())
    }

    async fn count(&self, sql: &str, param: &str) -> i64 {
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: sql.to_owned(),
                params: vec![param.into()],
            })
            .await
            .expect("count query")
            .expect("count row");
        match row.first() {
            Some(waddle_server::db::Value::Integer(count)) => *count,
            other => panic!("unexpected count {other:?}"),
        }
    }

    async fn close(self) {
        drop(self.actor);
        drop(self.db);
        if let Some(fixture) = self.postgres {
            fixture.close().await;
        }
    }
}

fn account(localpart: &str) -> BareJid {
    format!("{localpart}@{DOMAIN}")
        .parse()
        .expect("account JID")
}

fn register(username: &str) -> RegisterRequest {
    RegisterRequest {
        username: username.to_owned(),
        domain: DOMAIN.to_owned(),
        password: "correct-horse-battery".to_owned(),
        email: None,
    }
}

fn provider() -> AuthProviderConfig {
    AuthProviderConfig {
        id: "provider".to_owned(),
        display_name: "Provider".to_owned(),
        kind: AuthProviderKind::Oidc,
        dynamic_client_registration: false,
        client_id: "client".to_owned(),
        client_secret: "secret".to_owned(),
        token_endpoint_auth_method: AuthProviderTokenEndpointAuthMethod::ClientSecretPost,
        require_dpop: false,
        scopes: vec!["openid".to_owned()],
        issuer: Some("https://issuer.example".to_owned()),
        authorization_endpoint: None,
        token_endpoint: None,
        userinfo_endpoint: None,
        jwks_uri: None,
        subject_claim: "sub".to_owned(),
        username_claim: None,
        email_claim: Some("email".to_owned()),
    }
}

fn claims(subject: &str, preferred_username: &str) -> IdentityClaims {
    IdentityClaims {
        subject: subject.to_owned(),
        issuer: Some("https://issuer.example".to_owned()),
        preferred_username: Some(preferred_username.to_owned()),
        name: Some("Example".to_owned()),
        email: None,
        email_verified: None,
        avatar_url: None,
        raw_claims: json!({}),
    }
}

#[tokio::test]
async fn a_retired_native_account_is_never_registered_again() {
    for store in Store::each("retire_native").await {
        let engine = store.engine;
        let native = store.native();
        native.register(register("bob")).await.expect("register");
        assert!(native.user_exists("bob", DOMAIN).await.expect("lookup"));
        assert!(!jid_is_retired(&store.db, &account("bob"))
            .await
            .expect("ledger read"));

        let retirement = retire_account(&store.db, &account("bob"))
            .await
            .expect("retirement");
        assert_eq!(retirement.jid, "bob@example.com", "{engine}");
        assert!(retirement.newly_retired, "{engine}");
        assert_eq!(retirement.native_accounts, 1, "{engine}");
        assert!(!native.user_exists("bob", DOMAIN).await.expect("lookup"));
        assert!(jid_is_retired(&store.db, &account("bob"))
            .await
            .expect("ledger read"));

        // The JID is refused for good, and no row is created for it.
        for _ in 0..2 {
            assert!(
                matches!(
                    native.register(register("bob")).await,
                    Err(AuthError::JidRetired)
                ),
                "{engine}"
            );
        }
        assert!(!native.user_exists("bob", DOMAIN).await.expect("lookup"));
        // Another name is unaffected.
        native.register(register("bobby")).await.expect("register");

        // Retiring it again changes nothing.
        let again = retire_account(&store.db, &account("bob"))
            .await
            .expect("retirement");
        assert!(!again.newly_retired, "{engine}");
        assert_eq!((again.native_accounts, again.accounts), (0, 0), "{engine}");

        // A JID that never held an account is retired as well.
        let unused = retire_account(&store.db, &account("never"))
            .await
            .expect("retirement");
        assert!(unused.newly_retired, "{engine}");
        assert!(
            matches!(
                native.register(register("never")).await,
                Err(AuthError::JidRetired)
            ),
            "{engine}"
        );

        // The ledger is keyed by the canonical JID: another spelling of the
        // address retires the same account.
        native.register(register("dana")).await.expect("register");
        let spelled: BareJid = "Dana@Example.COM".parse().expect("JID");
        let retirement = retire_account(&store.db, &spelled)
            .await
            .expect("retirement");
        assert_eq!(retirement.jid, "dana@example.com", "{engine}");
        assert_eq!(retirement.native_accounts, 1, "{engine}");
        assert!(matches!(
            native.register(register("dana")).await,
            Err(AuthError::JidRetired)
        ));
        // A JID without a localpart names no account.
        assert!(matches!(
            retire_account(&store.db, &DOMAIN.parse().expect("domain JID")).await,
            Err(RetireAccountError::NotAnAccountJid)
        ));
        store.close().await;
    }
}

#[tokio::test]
async fn an_identity_provider_login_is_never_given_a_retired_jid() {
    for store in Store::each("retire_oidc").await {
        let engine = store.engine;
        let identities = IdentityService::new(store.actor.clone());
        let provider = provider();
        let first = identities
            .resolve_or_create_user(&provider, &claims("subject-1", "carol"), DOMAIN)
            .await
            .expect("first login");
        assert_eq!(first.user.jid, "carol@example.com", "{engine}");

        let retirement = retire_account(&store.db, &account("carol"))
            .await
            .expect("retirement");
        assert_eq!(retirement.accounts, 1, "{engine}");
        // The account's identity-provider link went with it.
        assert_eq!(
            store
                .count(
                    "SELECT count(*) FROM auth_identities WHERE user_jid = ?",
                    "carol@example.com"
                )
                .await,
            0,
            "{engine}"
        );

        // The same subject returns: it is provisioned as a new account
        // under another JID, never the retired one.
        let returned = identities
            .resolve_or_create_user(&provider, &claims("subject-1", "carol"), DOMAIN)
            .await
            .expect("login after retirement");
        assert_ne!(returned.user.jid, "carol@example.com", "{engine}");
        assert_eq!(returned.user.jid, "carol1@example.com", "{engine}");
        // So does another subject that asks for the same name.
        let other = identities
            .resolve_or_create_user(&provider, &claims("subject-2", "carol"), DOMAIN)
            .await
            .expect("another subject's login");
        assert_eq!(other.user.jid, "carol2@example.com", "{engine}");
        assert_eq!(
            store
                .count(
                    "SELECT count(*) FROM users WHERE jid = ?",
                    "carol@example.com"
                )
                .await,
            0,
            "{engine}"
        );
        store.close().await;
    }
}

#[tokio::test]
async fn no_session_and_no_account_row_is_created_for_a_retired_jid() {
    for store in Store::each("retire_session").await {
        let engine = store.engine;
        let sessions = SessionManager::new(store.actor.clone(), None);
        // A session of a native account creates the account's row.
        let session = Session::new_native_resume("erin@example.com", "erin", "erin");
        sessions.create_session(&session).await.expect("session");
        assert_eq!(
            store
                .count(
                    "SELECT count(*) FROM users WHERE jid = ?",
                    "erin@example.com"
                )
                .await,
            1,
            "{engine}"
        );

        let retirement = retire_account(&store.db, &account("erin"))
            .await
            .expect("retirement");
        assert_eq!(retirement.accounts, 1, "{engine}");
        // Its sessions went with the account row.
        assert_eq!(
            store
                .count(
                    "SELECT count(*) FROM sessions WHERE user_jid = ?",
                    "erin@example.com"
                )
                .await,
            0,
            "{engine}"
        );

        // A login that passed its credential check before the retirement
        // creates neither a session nor an account row afterwards.
        let late = Session::new_native_resume("erin@example.com", "erin", "erin");
        assert!(
            matches!(
                sessions.create_session(&late).await,
                Err(AuthError::JidRetired)
            ),
            "{engine}"
        );
        for table in ["users", "sessions"] {
            let column = if table == "users" { "jid" } else { "user_jid" };
            assert_eq!(
                store
                    .count(
                        &format!("SELECT count(*) FROM {table} WHERE {column} = ?"),
                        "erin@example.com"
                    )
                    .await,
                0,
                "{engine}: {table}"
            );
        }
        store.close().await;
    }
}

#[tokio::test]
async fn the_fixed_account_bootstrap_does_not_seed_a_retired_jid() {
    for store in Store::each("retire_fixed").await {
        let engine = store.engine;
        let config = FixedTestAccountConfig {
            username: "admin".to_owned(),
            password: "fixed-account-password".to_owned(),
            domain: DOMAIN.to_owned(),
            email: None,
        };
        // The bootstrap resets the account it is configured with.
        seed_fixed_test_account(&store.actor, &config)
            .await
            .expect("first seed");
        seed_fixed_test_account(&store.actor, &config)
            .await
            .expect("reset");
        retire_account(&store.db, &account("admin"))
            .await
            .expect("retirement");
        // Once the JID is retired the bootstrap fails instead of issuing it.
        assert!(
            seed_fixed_test_account(&store.actor, &config)
                .await
                .is_err(),
            "{engine}"
        );
        assert!(!store
            .native()
            .user_exists("admin", DOMAIN)
            .await
            .expect("lookup"));
        store.close().await;
    }
}

/// Wait until a backend of the store's database waits for a lock.
async fn wait_for_blocked_backend(observer: &sqlx::PgPool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let blocked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_catalog.pg_stat_activity \
             WHERE datname = current_database() AND wait_event_type = 'Lock' \
               AND cardinality(pg_catalog.pg_blocking_pids(pid)) > 0",
        )
        .fetch_one(observer)
        .await
        .expect("lock observation");
        if blocked > 0 {
            return;
        }
        assert!(Instant::now() < deadline, "no backend waited for a lock");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_registration_that_waits_for_a_retirement_is_refused() {
    let Some(store) = Store::postgres("retire_then_register").await else {
        return;
    };
    let observer = sqlx::PgPool::connect(store.db.database_url())
        .await
        .expect("observer connection");
    // A retirement that has locked the account tables and recorded the JID,
    // and has not committed.
    let mut retiring = observer.begin().await.expect("retiring transaction");
    sqlx::query("LOCK TABLE users, native_users IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *retiring)
        .await
        .expect("lock");
    sqlx::query(
        "INSERT INTO retired_account_jids (jid_key, localpart_key, retired_at_ms) \
         VALUES ('frank@example.com', 'frank', 0)",
    )
    .execute(&mut *retiring)
    .await
    .expect("ledger row");

    // A registration of that JID arrives. It cannot insert beside the
    // retirement, and once the retirement commits it runs against the
    // ledger that holds the JID.
    let native = store.native();
    let registration = tokio::spawn(async move { native.register(register("frank")).await });
    wait_for_blocked_backend(&observer).await;
    assert!(!registration.is_finished());
    retiring.commit().await.expect("retirement commits");
    assert!(matches!(
        registration.await.expect("registration task"),
        Err(AuthError::JidRetired)
    ));
    assert!(!store
        .native()
        .user_exists("frank", DOMAIN)
        .await
        .expect("lookup"));
    observer.close().await;
    store.close().await;
}

#[tokio::test]
async fn a_retirement_waits_for_a_registration_and_removes_its_account() {
    let Some(store) = Store::postgres("register_then_retire").await else {
        return;
    };
    let observer = sqlx::PgPool::connect(store.db.database_url())
        .await
        .expect("observer connection");
    // A registration that has inserted its account and has not committed.
    let mut registering = observer.begin().await.expect("registering transaction");
    sqlx::query(
        "INSERT INTO native_users (username, domain, jid_key, password_hash, salt, stored_key, \
         server_key) VALUES ('grace', 'example.com', 'grace@example.com', 'hash', 'c2FsdA==', \
         '\\x00', '\\x00')",
    )
    .execute(&mut *registering)
    .await
    .expect("uncommitted account");

    // The retirement waits for it instead of recording the JID beside an
    // account it cannot see, and then removes the account with the JID.
    let db = store.db.clone();
    let retirement = tokio::spawn(async move { retire_account(&db, &account("grace")).await });
    wait_for_blocked_backend(&observer).await;
    assert!(!retirement.is_finished());
    registering.commit().await.expect("registration commits");
    let retirement = retirement
        .await
        .expect("retirement task")
        .expect("retirement");
    assert!(retirement.newly_retired);
    assert_eq!(retirement.native_accounts, 1);
    assert!(!store
        .native()
        .user_exists("grace", DOMAIN)
        .await
        .expect("lookup"));
    observer.close().await;
    store.close().await;
}

#[tokio::test]
async fn a_retirement_that_cannot_lock_the_account_tables_in_time_changes_nothing() {
    let Some(store) = Store::postgres("retire_lock_bound").await else {
        return;
    };
    let observer = sqlx::PgPool::connect(store.db.database_url())
        .await
        .expect("observer connection");
    // A registration that has inserted its account and stays uncommitted
    // for longer than a retirement waits for the account tables.
    let mut registering = observer.begin().await.expect("registering transaction");
    sqlx::query(
        "INSERT INTO native_users (username, domain, jid_key, password_hash, salt, stored_key, \
         server_key) VALUES ('heidi', 'example.com', 'heidi@example.com', 'hash', 'c2FsdA==', \
         '\\x00', '\\x00')",
    )
    .execute(&mut *registering)
    .await
    .expect("uncommitted account");

    // The retirement gives up instead of holding up every later writer of
    // the account tables behind its lock request.
    let started = Instant::now();
    let refused = retire_account(&store.db, &account("heidi")).await;
    let waited = started.elapsed();
    assert!(
        matches!(refused, Err(RetireAccountError::Database(_))),
        "{refused:?}"
    );
    assert!(
        waited >= Duration::from_millis(1900),
        "the retirement gave up after {waited:?}"
    );

    // It recorded nothing, and the registration it waited for completes.
    assert!(!jid_is_retired(&store.db, &account("heidi"))
        .await
        .expect("ledger read"));
    registering.commit().await.expect("registration commits");
    assert!(store
        .native()
        .user_exists("heidi", DOMAIN)
        .await
        .expect("lookup"));

    // Run again, it retires the account.
    let retirement = retire_account(&store.db, &account("heidi"))
        .await
        .expect("retirement");
    assert!(retirement.newly_retired);
    assert_eq!(retirement.native_accounts, 1);
    assert!(jid_is_retired(&store.db, &account("heidi"))
        .await
        .expect("ledger read"));
    observer.close().await;
    store.close().await;
}

/// Run `waddle-server account retire` with `args` and `env`, and answer its
/// exit code and what it printed.
async fn account_retire(args: &[&str], env: &[(&str, &str)]) -> (i32, serde_json::Value, String) {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_waddle-server"));
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy().into_owned();
        if key.starts_with("WADDLE_") || key.starts_with("OTEL_") || key.starts_with("PG") {
            command.env_remove(key);
        }
    }
    command
        .args(["account", "retire"])
        .args(args)
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = tokio::time::timeout(Duration::from_secs(120), command.output())
        .await
        .expect("the subcommand exits")
        .expect("the subcommand runs");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let printed = serde_json::from_str(stdout.trim()).unwrap_or(serde_json::Value::Null);
    (
        output.status.code().expect("exit code"),
        printed,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[tokio::test]
async fn the_operator_subcommand_retires_an_account_on_sqlite() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("waddle.db");
    let db = Database::from_config(
        "retire-cli",
        &DatabaseConfig::new(
            DatabaseDriver::Sqlite,
            format!("sqlite://{}?mode=rwc", path.display()),
        ),
    )
    .await
    .expect("SQLite database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrations");
    let actor = DbActor::spawn(DbActor::new(db.clone()));
    let native = NativeUserStore::new(actor.clone());
    native.register(register("heidi")).await.expect("register");

    let file = path.to_string_lossy().into_owned();
    let (code, printed, stderr) = account_retire(
        &["--jid", "heidi@example.com", "--sqlite-database", &file],
        &[],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(printed["jid"], "heidi@example.com");
    assert_eq!(printed["newly_retired"], true);
    assert_eq!(printed["native_accounts_removed"], 1);
    assert!(matches!(
        native.register(register("heidi")).await,
        Err(AuthError::JidRetired)
    ));

    // Repeating it reports the JID as already retired; a JID without a
    // localpart and a missing file are usage errors that change nothing.
    let (code, printed, _) = account_retire(
        &["--jid", "heidi@example.com", "--sqlite-database", &file],
        &[],
    )
    .await;
    assert_eq!(code, 0);
    assert_eq!(printed["newly_retired"], false);
    let (code, _, _) =
        account_retire(&["--jid", "example.com", "--sqlite-database", &file], &[]).await;
    assert_eq!(code, 2);
    let missing = directory.path().join("absent.db");
    let (code, _, _) = account_retire(
        &[
            "--jid",
            "ivan@example.com",
            "--sqlite-database",
            &missing.to_string_lossy(),
        ],
        &[],
    )
    .await;
    assert_eq!(code, 2);
    assert!(!missing.exists());
    drop((native, actor, db));
}

#[tokio::test]
async fn the_operator_subcommand_retires_an_account_on_postgres() {
    let Some(store) = Store::postgres("retire_cli").await else {
        return;
    };
    let native = store.native();
    native.register(register("judy")).await.expect("register");
    // The fixture's login owns the schema, so it is both the login the act
    // runs as and the role it acts as.
    let url = store.db.database_url().to_owned();
    let login = url::Url::parse(&url)
        .expect("database URL")
        .username()
        .to_owned();
    let (code, printed, stderr) = account_retire(
        &[
            "--jid",
            "judy@example.com",
            "--schema",
            "public",
            "--migrator-login",
            &login,
            "--schema-owner",
            &login,
        ],
        &[("WADDLE_DATABASE_URL", &url)],
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(printed["jid"], "judy@example.com");
    assert_eq!(printed["newly_retired"], true);
    assert_eq!(printed["native_accounts_removed"], 1);
    assert!(matches!(
        native.register(register("judy")).await,
        Err(AuthError::JidRetired)
    ));
    // Without a database the subcommand names what it needs and exits as a
    // usage error.
    let (code, _, _) = account_retire(&["--jid", "judy@example.com"], &[]).await;
    assert_eq!(code, 2);
    drop(native);
    store.close().await;
}

/// Every provisioning path refuses a retired JID and leaves its one ledger
/// row as it is.
#[tokio::test]
async fn the_ledger_is_never_updated_or_deleted_by_provisioning() {
    for store in Store::each("retire_ledger").await {
        let engine = store.engine;
        retire_account(&store.db, &account("kim"))
            .await
            .expect("retirement");
        let native = store.native();
        let identities = IdentityService::new(store.actor.clone());
        let _ = native.register(register("kim")).await;
        let _ = identities
            .resolve_or_create_user(&provider(), &claims("subject-9", "kim"), DOMAIN)
            .await
            .expect("login");
        let _ = SessionManager::new(store.actor.clone(), None)
            .create_session(&Session::new_native_resume("kim@example.com", "kim", "kim"))
            .await;
        assert_eq!(
            store
                .count(
                    "SELECT count(*) FROM retired_account_jids WHERE jid_key = ?",
                    "kim@example.com"
                )
                .await,
            1,
            "{engine}"
        );
        assert!(jid_is_retired(&store.db, &account("kim"))
            .await
            .expect("ledger read"));
        store.close().await;
    }
}

#[tokio::test]
async fn retirement_removes_what_remote_senders_left_for_the_account() {
    use waddle_server::db::s2s_accounts::{
        OfflineAdmission, PendingOutcome, PendingSubscriptions, RemoteOfflineAdmissions,
        RemotePubSubSubscriptions, SubscriptionOutcome,
    };
    use xmpp_parsers::presence::{Presence, Type as PresenceType};

    const TABLES: [&str; 3] = [
        "SELECT count(*) FROM s2s_pending_subscriptions WHERE account_jid = ?",
        "SELECT count(*) FROM s2s_remote_pubsub_subscriptions WHERE account_jid = ?",
        "SELECT count(*) FROM s2s_remote_offline_admissions WHERE account_jid = ?",
    ];
    for store in Store::each("retire_s2s").await {
        let engine = store.engine;
        let lee = account("lee");
        let contact: BareJid = "carol@remote.test".parse().expect("a JID");
        let service: BareJid = "pubsub.remote.test".parse().expect("a JID");
        let now = chrono::Utc::now().timestamp_millis();
        let mut request = Presence::new(PresenceType::Subscribe);
        request.from = Some(contact.clone().into());
        request.to = Some(lee.clone().into());
        assert_eq!(
            PendingSubscriptions::new(store.db.clone())
                .record(&lee, &contact, &request.into(), now)
                .await
                .expect("a recorded request"),
            PendingOutcome::Recorded,
            "{engine}"
        );
        assert_eq!(
            RemotePubSubSubscriptions::new(store.db.clone())
                .record(&lee, &service, "news", now)
                .await
                .expect("a recorded subscription"),
            SubscriptionOutcome::Recorded,
            "{engine}"
        );
        assert_eq!(
            RemoteOfflineAdmissions::new(store.db.clone())
                .admit(&lee, &contact, now)
                .await
                .expect("an admission"),
            OfflineAdmission::Admitted,
            "{engine}"
        );
        for sql in TABLES {
            assert_eq!(
                store.count(sql, "lee@example.com").await,
                1,
                "{engine}: {sql}"
            );
        }

        retire_account(&store.db, &lee).await.expect("retirement");

        for sql in TABLES {
            assert_eq!(
                store.count(sql, "lee@example.com").await,
                0,
                "{engine}: {sql}"
            );
        }
        store.close().await;
    }
}

#[tokio::test]
async fn retirement_empties_the_offline_store_under_every_spelling_of_the_account() {
    use waddle_server::db::actor::DbExecute;
    use waddle_server::pending_delivery::DatabasePendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId, QuotaPolicy};
    use xmpp_parsers::message::{Lang, Message};

    /// A message from Bob that the offline store keeps whole for `to`.
    fn row(to: &BareJid) -> PendingRow {
        let mut message = Message::new(Some(jid::Jid::from(to.clone())));
        message.from = Some("bob@example.com/phone".parse().expect("Bob's JID"));
        message.bodies.insert(Lang::new(), "later".to_owned());
        PendingRow {
            id: PendingRowId::fresh(),
            recipient: to.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Transient(Box::new(message)),
            flushed_in_session: None,
            outbound_sequence: None,
        }
    }
    const COUNT: &str = "SELECT count(*) FROM pending_delivery WHERE recipient_jid = ?";

    for store in Store::each("retire_offline_spellings").await {
        let engine = store.engine;
        let offline = match &store.postgres {
            Some(fixture) => DatabasePendingDeliveryStorage::open(
                Some(fixture.db.database_url()),
                QuotaPolicy::Unlimited,
            )
            .await
            .expect("the offline store"),
            None => DatabasePendingDeliveryStorage::attach_existing(
                store.db.clone(),
                QuotaPolicy::Unlimited,
            )
            .await
            .expect("the offline store"),
        };
        // The account, at a domain with a U-label, and its localpart holding
        // a character a `LIKE` pattern reads as a wildcard.
        let canonical: BareJid = "al_ce@xn--bcher-kva.example".parse().expect("a JID");
        // The `jid` parser keeps the U-label: another spelling of the
        // account.
        let unicode: BareJid = "al_ce@b\u{fc}cher.example".parse().expect("a JID");
        assert_eq!(unicode.as_str(), "al_ce@b\u{fc}cher.example");
        // A localpart the wildcard would also match, and the account's
        // localpart at another domain: neither is the account.
        let lookalike: BareJid = "alxce@xn--bcher-kva.example".parse().expect("a JID");
        let elsewhere: BareJid = "al_ce@example.com".parse().expect("a JID");
        for recipient in [&canonical, &unicode, &canonical, &lookalike, &elsewhere] {
            offline
                .insert(row(recipient))
                .await
                .expect("a stored message");
        }
        // A row written before recipients were prepared, in another letter
        // case.
        let legacy = "AL_CE@XN--BCHER-KVA.EXAMPLE";
        let rewritten = store
            .actor
            .ask(DbExecute {
                sql: "UPDATE pending_delivery SET recipient_jid = ? WHERE row_id = \
                      (SELECT min(row_id) FROM pending_delivery WHERE recipient_jid = ?)"
                    .to_owned(),
                params: vec![legacy.into(), canonical.as_str().into()],
            })
            .await
            .expect("a legacy spelling");
        assert_eq!(rewritten, 1, "{engine}");

        let retirement = retire_account(&store.db, &canonical)
            .await
            .expect("retirement");
        assert_eq!(retirement.offline_messages, 3, "{engine}");
        for gone in [canonical.as_str(), unicode.as_str(), legacy] {
            assert_eq!(store.count(COUNT, gone).await, 0, "{engine}: {gone}");
        }
        for kept in [lookalike.as_str(), elsewhere.as_str()] {
            assert_eq!(store.count(COUNT, kept).await, 1, "{engine}: {kept}");
        }
        drop(offline);
        store.close().await;
    }
}
