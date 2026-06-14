use super::local_account_exists;
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
                  (id, username, xmpp_localpart, display_name, avatar_url, primary_email, created_at, updated_at) \
                  VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
                .to_string(),
            params: vec![
                format!("id-{localpart}").into(),
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
