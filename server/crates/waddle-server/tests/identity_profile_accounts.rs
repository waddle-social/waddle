//! The identity profile at the account stores: canonical configured
//! domains, exact provisioning and lookup, and the reconciliation report for
//! accounts written before the profile. In-memory SQLite, no server process.

use kameo::actor::{ActorRef, Spawn};
use waddle_server::auth::identity_profile::{
    reconcile_identity_profile, require_canonical_domains, AccountFinding, AccountRecord,
};
use waddle_server::auth::{local_account_exists, AuthError, NativeUserStore, RegisterRequest};
use waddle_server::db::actor::{DbActor, DbExecute};
use waddle_server::db::{Database, MigrationRunner, Value};
use waddle_xmpp::identity::{CanonicalDomain, IdentityError};

const DOMAIN: &str = "localhost";

async fn accounts_actor(name: &str) -> ActorRef<DbActor> {
    let db = Database::in_memory(name).await.expect("in-memory database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrations");
    DbActor::spawn(DbActor::new(db))
}

async fn execute(actor: &ActorRef<DbActor>, sql: &str, params: Vec<Value>) {
    actor
        .ask(DbExecute {
            sql: sql.to_owned(),
            params,
        })
        .await
        .expect("seed row");
}

/// An OIDC row as an older node wrote it: the stored JID, localpart and the
/// key that node computed.
async fn seed_oidc(actor: &ActorRef<DbActor>, jid: &str, localpart: &str, key: Option<&str>) {
    execute(
        actor,
        "INSERT INTO users (jid, username, xmpp_localpart, localpart_key, created_at, updated_at) \
         VALUES (?, ?, ?, ?, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        vec![
            jid.into(),
            localpart.into(),
            localpart.into(),
            key.map_or(Value::NullText, Value::from),
        ],
    )
    .await;
}

/// A native row as an older node wrote it, bypassing registration checks.
async fn seed_native(actor: &ActorRef<DbActor>, username: &str, domain: &str, key: Option<&str>) {
    execute(
        actor,
        "INSERT INTO native_users (username, domain, jid_key, password_hash, salt, stored_key, server_key) \
         VALUES (?, ?, ?, 'hash', 'c2FsdA==', x'00', x'00')",
        vec![
            username.into(),
            domain.into(),
            key.map_or(Value::NullText, Value::from),
        ],
    )
    .await;
}

async fn native_id(actor: &ActorRef<DbActor>, username: &str) -> i64 {
    let row = actor
        .ask(waddle_server::db::actor::DbQueryOne {
            sql: "SELECT id FROM native_users WHERE username = ?".to_owned(),
            params: vec![username.into()],
        })
        .await
        .expect("query")
        .expect("native row");
    match row.first() {
        Some(Value::Integer(id)) => *id,
        other => panic!("unexpected id {other:?}"),
    }
}

fn register(username: &str) -> RegisterRequest {
    RegisterRequest {
        username: username.to_owned(),
        domain: DOMAIN.to_owned(),
        password: "correct-horse-battery".to_owned(),
        email: None,
    }
}

#[test]
fn configured_domains_must_already_be_canonical() {
    let domain = require_canonical_domains("example.com", &["muc.example.com"]).expect("canonical");
    assert_eq!(domain.as_str(), "example.com");
    assert_eq!(
        require_canonical_domains("xn--bcher-kva.example", &[])
            .expect("A-label domain")
            .as_str(),
        "xn--bcher-kva.example"
    );
    for (account, service) in [
        ("Example.com", "muc.example.com"),
        ("example.com.", "muc.example.com"),
        ("bücher.example", "muc.example.com"),
        ("example.com", "MUC.example.com"),
        ("example.com", "muc.example.com."),
    ] {
        assert!(
            matches!(
                require_canonical_domains(account, &[service]),
                Err(IdentityError::NonCanonicalDomain)
            ),
            "{account} / {service}"
        );
    }
}

#[tokio::test]
async fn provisioning_accepts_only_the_account_profile() {
    let actor = accounts_actor("identity-profile-provisioning").await;
    let store = NativeUserStore::new(actor.clone());
    store
        .register(register("alice+work"))
        .await
        .expect("account profile name");
    for refused in ["Alice", "äda", "ａｌｉｃｅ", "\u{fb01}x", "a b", ""] {
        assert!(
            matches!(
                store.register(register(refused)).await,
                Err(AuthError::InvalidUsername(_))
            ),
            "{refused:?}"
        );
    }
    let mut foreign = register("bob");
    foreign.domain = "Localhost".to_owned();
    assert!(matches!(
        store.register(foreign).await,
        Err(AuthError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn login_and_directory_lookup_take_names_exactly() {
    let actor = accounts_actor("identity-profile-lookup").await;
    let store = NativeUserStore::new(actor.clone());
    store.register(register("ada")).await.expect("register ada");
    // A legacy native `Bob` keyed under `bob`, and a legacy OIDC `straße`
    // keyed under `strasse`, as nodes before the profile wrote them.
    seed_native(&actor, "Bob", DOMAIN, Some("bob@localhost")).await;
    seed_oidc(&actor, "straße@localhost", "straße", Some("strasse")).await;

    for (username, found) in [("ada", true), ("ADA", false), ("Ada", false)] {
        assert_eq!(
            store
                .get_scram_credentials(username, DOMAIN)
                .await
                .expect("lookup")
                .is_some(),
            found,
            "{username}"
        );
    }
    // The legacy account signs in only under the spelling its key records.
    assert!(store
        .get_scram_credentials("bob", DOMAIN)
        .await
        .expect("lookup")
        .is_some());
    assert!(store
        .get_scram_credentials("Bob", DOMAIN)
        .await
        .expect("lookup")
        .is_none());

    for (name, exists) in [
        ("ada", true),
        ("bob", true),
        ("strasse", true),
        ("Ada", false),
        ("BOB", false),
        ("straße", false),
        ("ａｄａ", false),
        ("ada\u{ad}", false),
    ] {
        assert_eq!(
            local_account_exists(&actor, name, DOMAIN)
                .await
                .expect("directory lookup"),
            exists,
            "{name:?}"
        );
    }
}

#[tokio::test]
async fn reconciliation_reports_accounts_outside_the_profile() {
    let actor = accounts_actor("identity-profile-reconciliation").await;
    let domain = CanonicalDomain::parse_exact(DOMAIN).expect("canonical domain");
    NativeUserStore::new(actor.clone())
        .register(register("clean"))
        .await
        .expect("register clean account");
    seed_oidc(&actor, "alice@localhost", "alice", Some("alice")).await;
    // Canonical but outside the ASCII account profile.
    seed_oidc(&actor, "josé@localhost", "josé", Some("josé")).await;
    // Spellings preparation would change.
    seed_oidc(&actor, "straße@localhost", "straße", Some("strasse")).await;
    seed_native(&actor, "Carol", DOMAIN, Some("carol@localhost")).await;
    // Unkeyed, and folding onto the OIDC `alice` across the stores.
    seed_native(&actor, "alice", DOMAIN, None).await;
    // Another deployment's domain.
    seed_native(
        &actor,
        "remote",
        "elsewhere.test",
        Some("remote@elsewhere.test"),
    )
    .await;
    // A recorded key that is not the stored name's key.
    seed_native(&actor, "dave", DOMAIN, Some("eve@localhost")).await;

    let report = reconcile_identity_profile(&actor, &domain)
        .await
        .expect("reconciliation");
    assert_eq!(report.accounts, 8);
    assert!(!report.is_clean());

    let native = |username: &'static str| {
        let actor = actor.clone();
        async move {
            AccountRecord::Native {
                id: native_id(&actor, username).await,
            }
        }
    };
    let oidc = |jid: &str| AccountRecord::Oidc {
        jid: jid.to_owned(),
    };
    let clean = native("clean").await;
    assert_eq!(report.findings(&clean), None);
    assert_eq!(
        report.findings(&oidc("josé@localhost")),
        Some(&[AccountFinding::OutsideAccountProfile][..])
    );
    assert_eq!(
        report.findings(&oidc("straße@localhost")),
        Some(&[AccountFinding::NonCanonicalSpelling][..])
    );
    assert_eq!(
        report.findings(&native("Carol").await),
        Some(&[AccountFinding::NonCanonicalSpelling][..])
    );
    assert_eq!(
        report.findings(&native("alice").await),
        Some(&[AccountFinding::MissingKey, AccountFinding::FoldedCollision][..])
    );
    assert_eq!(
        report.findings(&oidc("alice@localhost")),
        Some(&[AccountFinding::FoldedCollision][..])
    );
    assert_eq!(
        report.findings(&native("remote").await),
        Some(&[AccountFinding::ForeignDomain][..])
    );
    assert_eq!(
        report.findings(&native("dave").await),
        Some(&[AccountFinding::KeyMismatch][..])
    );
    assert_eq!(report.count(AccountFinding::FoldedCollision), 2);
    assert_eq!(report.outside.len(), 7);

    // The report is read only: running it again finds the same.
    assert_eq!(
        reconcile_identity_profile(&actor, &domain)
            .await
            .expect("second reconciliation"),
        report
    );
}
