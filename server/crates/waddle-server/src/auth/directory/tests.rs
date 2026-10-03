use super::{canonical_localpart, local_account_exists, local_account_jid_exists};
use crate::auth::{NativeUserStore, RegisterRequest};
use crate::db::actor::{DbActor, DbExecute};
use crate::db::{Database, MigrationRunner};
use kameo::actor::{ActorRef, Spawn};
use std::sync::Arc;

async fn test_actor() -> ActorRef<DbActor> {
    let db = Database::in_memory("test-local-directory")
        .await
        .expect("create test database");
    let db = Arc::new(db);
    MigrationRunner::global()
        .run(&db)
        .await
        .expect("run migrations");
    DbActor::spawn(DbActor::new((*db).clone()))
}

/// Insert an OIDC-provisioned identity directly into the `users` table, the
/// way the OIDC login flow does (see `auth/identity.rs::create_user`).
async fn seed_oidc_user(actor: &ActorRef<DbActor>, localpart: &str) {
    actor
        .ask(DbExecute {
            sql: "INSERT INTO users \
                  (jid, username, xmpp_localpart, localpart_key, display_name, avatar_url, primary_email, created_at, updated_at) \
                  VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
                .to_string(),
            params: vec![
                format!("{localpart}@localhost").into(),
                localpart.into(),
                localpart.into(),
                localpart.into(),
                "Test User".into(),
                crate::db::Value::NullText,
                crate::db::Value::NullText,
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ],
        })
        .await
        .expect("seed oidc user");
}

/// An OIDC user lives only in `users`, never in `native_users`. The unified
/// directory check must still recognise it — this is the exact regression
/// behind "group-DM member does not exist" for web-registered accounts.
#[tokio::test]
async fn finds_oidc_only_user() {
    let actor = test_actor().await;
    seed_oidc_user(&actor, "icepuma").await;

    // The native-only check is blind to OIDC accounts...
    let native_only = NativeUserStore::new(actor.clone())
        .user_exists("icepuma", "localhost")
        .await
        .expect("native lookup");
    assert!(!native_only, "OIDC user must be absent from native_users");

    // ...but the unified directory check sees it.
    let exists = local_account_exists(&actor, "icepuma", "localhost")
        .await
        .expect("directory lookup");
    assert!(exists, "OIDC user must be recognised as a local account");
}

#[tokio::test]
async fn finds_native_user() {
    let actor = test_actor().await;
    NativeUserStore::new(actor.clone())
        .register(RegisterRequest {
            username: "rawkode".to_string(),
            domain: "localhost".to_string(),
            password: "rawkode-pass-1234".to_string(),
            email: None,
        })
        .await
        .expect("register native user");

    let exists = local_account_exists(&actor, "rawkode", "localhost")
        .await
        .expect("directory lookup");
    assert!(exists, "native user must be recognised as a local account");
}

#[tokio::test]
async fn false_for_unknown_account() {
    let actor = test_actor().await;
    let exists = local_account_exists(&actor, "nobody", "localhost")
        .await
        .expect("directory lookup");
    assert!(!exists, "unknown localpart must not resolve to an account");
}

/// Native accounts are keyed by `(username, domain)`; a matching localpart on
/// a different domain must not be treated as the same account.
#[tokio::test]
async fn native_match_is_domain_scoped() {
    let actor = test_actor().await;
    NativeUserStore::new(actor.clone())
        .register(RegisterRequest {
            username: "frank".to_string(),
            domain: "example.com".to_string(),
            password: "frank-pass-1234".to_string(),
            email: None,
        })
        .await
        .expect("register native user");

    let other_domain = local_account_exists(&actor, "frank", "other.test")
        .await
        .expect("directory lookup");
    assert!(
        !other_domain,
        "native account must not match across domains"
    );
}

/// Account names match on the JID library's canonical localpart, not SQL
/// `lower()`: a native `Äda` is `äda@localhost` (non-ASCII case folding) and
/// an OIDC `straße` is `strasse@localhost` (nodeprep maps ß to ss).
#[tokio::test]
async fn matches_names_on_their_canonical_localpart() {
    let actor = test_actor().await;
    NativeUserStore::new(actor.clone())
        .register(RegisterRequest {
            username: "Äda".to_string(),
            domain: "localhost".to_string(),
            password: format!("{:x}", rand::random::<u64>()),
            email: None,
        })
        .await
        .expect("register native user");
    actor
        .ask(DbExecute {
            sql: "INSERT INTO users \
                  (jid, username, xmpp_localpart, localpart_key, created_at, updated_at) \
                  VALUES (?, ?, ?, ?, ?, ?)"
                .to_string(),
            params: vec![
                "straße@localhost".into(),
                "straße".into(),
                "straße".into(),
                canonical_localpart("straße")
                    .expect("valid localpart")
                    .as_str()
                    .into(),
                "2026-01-01T00:00:00Z".into(),
                "2026-01-01T00:00:00Z".into(),
            ],
        })
        .await
        .expect("seed oidc user");

    for name in ["äda", "Äda", "ÄDA", "strasse", "STRASSE", "straße"] {
        assert!(
            local_account_exists(&actor, name, "localhost")
                .await
                .expect("directory lookup"),
            "{name} names an account"
        );
    }
    let jid: jid::BareJid = "äda@localhost".parse().expect("jid");
    assert!(local_account_jid_exists(&actor, &jid, "localhost")
        .await
        .expect("directory lookup"));
    assert!(!local_account_exists(&actor, "ada", "localhost")
        .await
        .expect("directory lookup"));
}

/// The existence lookup searches both key indexes instead of scanning a
/// table per check, which `lower(username)` forced.
#[tokio::test]
async fn existence_lookup_searches_the_key_indexes() {
    let db = Database::in_memory("test-local-directory-plan")
        .await
        .expect("create test database");
    MigrationRunner::global()
        .run(&db)
        .await
        .expect("run migrations");
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn
        .query(
            &format!("EXPLAIN QUERY PLAN {}", super::ACCOUNT_EXISTS_SQL),
            crate::db_params!["äda", "äda@localhost"],
        )
        .await
        .expect("query plan");
    let mut plan = Vec::new();
    while let Some(row) = rows.next().await.expect("plan row") {
        plan.push(row.get::<String>(3).expect("plan detail"));
    }
    let plan = plan.join("\n");
    assert!(plan.contains("idx_users_localpart_key"), "{plan}");
    assert!(plan.contains("idx_native_users_jid_key"), "{plan}");
    assert!(!plan.contains("SCAN"), "{plan}");
}

/// Rows a node predating the lookup keys writes while this one serves get
/// their keys from the periodic pass, without a restart.
#[tokio::test]
async fn periodic_backfill_keys_rows_written_without_keys() {
    let actor = test_actor().await;
    for sql in [
        "INSERT INTO native_users (username, domain, password_hash, salt, stored_key, server_key) \
         VALUES ('Bob', 'localhost', 'hash', 'salt', '', '')",
        "INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) \
         VALUES ('straße@localhost', 'straße', 'straße', 'now', 'now')",
    ] {
        actor
            .ask(DbExecute {
                sql: sql.to_string(),
                params: vec![],
            })
            .await
            .expect("unkeyed account");
    }
    let found = |name: &'static str| {
        let actor = actor.clone();
        async move {
            local_account_exists(&actor, name, "localhost")
                .await
                .expect("directory lookup")
        }
    };
    assert!(!found("bob").await && !found("strasse").await);

    super::spawn_account_key_backfill(&actor, std::time::Duration::from_millis(10));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !(found("bob").await && found("strasse").await) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the periodic pass keys both accounts");
}
