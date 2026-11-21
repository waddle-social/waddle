use super::{
    validate_username, AuthError, DbExecute, NativeUserStore, RegisterRequest,
    DEFAULT_SCRAM_ITERATIONS,
};
use crate::db::{Database, MigrationRunner};
use kameo::actor::Spawn;
use std::sync::Arc;

fn test_password() -> String {
    format!("{:x}{:x}", rand::random::<u64>(), rand::random::<u64>())
}

async fn create_test_db() -> Arc<Database> {
    let db = Database::in_memory("test-native-users")
        .await
        .expect("Failed to create test database");
    let db = Arc::new(db);

    // Run migrations
    let runner = MigrationRunner::global();
    runner.run(&db).await.expect("Failed to run migrations");

    db
}

#[tokio::test]
async fn test_register_user() {
    let db = create_test_db().await;
    let actor = crate::db::actor::DbActor::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let store = NativeUserStore::new(actor);
    let password = test_password();

    let request = RegisterRequest {
        username: "alice".to_string(),
        domain: "example.com".to_string(),
        password,
        email: Some("alice@email.com".to_string()),
    };

    let user_id = store
        .register(request)
        .await
        .expect("Failed to register user");
    assert!(user_id > 0);

    // Verify user exists
    let exists = store.user_exists("alice", "example.com").await.unwrap();
    assert!(exists);
}

#[tokio::test]
async fn test_duplicate_user() {
    let db = create_test_db().await;
    let actor = crate::db::actor::DbActor::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let store = NativeUserStore::new(actor);
    let password = test_password();

    let request = RegisterRequest {
        username: "bob".to_string(),
        domain: "example.com".to_string(),
        password,
        email: None,
    };

    store
        .register(request.clone())
        .await
        .expect("First registration should succeed");

    // Second registration should fail
    let result = store.register(request).await;
    assert!(matches!(result, Err(AuthError::UserAlreadyExists(_))));
}

#[tokio::test]
async fn test_get_scram_credentials() {
    let db = create_test_db().await;
    let actor = crate::db::actor::DbActor::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let store = NativeUserStore::new(actor);
    let password = test_password();

    let request = RegisterRequest {
        username: "charlie".to_string(),
        domain: "example.com".to_string(),
        password,
        email: None,
    };

    store.register(request).await.unwrap();

    let creds = store
        .get_scram_credentials("charlie", "example.com")
        .await
        .unwrap();

    assert!(creds.is_some());
    let creds = creds.unwrap();

    // Verify SCRAM keys are properly generated
    assert_eq!(creds.stored_key.len(), 32); // SHA-256 output
    assert_eq!(creds.server_key.len(), 32);
    assert_eq!(creds.iterations, DEFAULT_SCRAM_ITERATIONS);
    assert!(!creds.salt_b64.is_empty());
}

#[tokio::test]
async fn test_verify_password() {
    let db = create_test_db().await;
    let actor = crate::db::actor::DbActor::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let store = NativeUserStore::new(actor);
    let correct_password = test_password();
    let wrong_password = test_password();
    let missing_password = test_password();

    let request = RegisterRequest {
        username: "dave".to_string(),
        domain: "example.com".to_string(),
        password: correct_password.clone(),
        email: None,
    };

    store.register(request).await.unwrap();

    // Correct password should verify
    let verified = store
        .verify_password("dave", "example.com", &correct_password)
        .await
        .unwrap();
    assert!(verified);

    // Wrong password should not verify
    let verified = store
        .verify_password("dave", "example.com", &wrong_password)
        .await
        .unwrap();
    assert!(!verified);

    // Non-existent user should not verify
    let verified = store
        .verify_password("nonexistent", "example.com", &missing_password)
        .await
        .unwrap();
    assert!(!verified);
}

#[tokio::test]
async fn test_update_password() {
    let db = create_test_db().await;
    let actor = crate::db::actor::DbActor::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let store = NativeUserStore::new(actor);
    let old_password = test_password();
    let new_password = test_password();

    let request = RegisterRequest {
        username: "eve".to_string(),
        domain: "example.com".to_string(),
        password: old_password.clone(),
        email: None,
    };

    store.register(request).await.unwrap();

    // Update password
    store
        .update_password("eve", "example.com", &new_password)
        .await
        .unwrap();

    // Old password should not work
    let verified = store
        .verify_password("eve", "example.com", &old_password)
        .await
        .unwrap();
    assert!(!verified);

    // New password should work
    let verified = store
        .verify_password("eve", "example.com", &new_password)
        .await
        .unwrap();
    assert!(verified);
}

#[tokio::test]
async fn test_delete_user() {
    let db = create_test_db().await;
    let actor = crate::db::actor::DbActor::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let store = NativeUserStore::new(actor);
    let password = test_password();

    let request = RegisterRequest {
        username: "frank".to_string(),
        domain: "example.com".to_string(),
        password,
        email: None,
    };

    store.register(request).await.unwrap();

    // Delete user
    let deleted = store.delete_user("frank", "example.com").await.unwrap();
    assert!(deleted);

    // User should no longer exist
    let exists = store.user_exists("frank", "example.com").await.unwrap();
    assert!(!exists);

    // Deleting again should return false
    let deleted = store.delete_user("frank", "example.com").await.unwrap();
    assert!(!deleted);
}

#[tokio::test]
async fn test_validate_username() {
    // Valid usernames
    assert!(validate_username("alice").is_ok());
    assert!(validate_username("alice123").is_ok());
    assert!(validate_username("alice.bob").is_ok());
    assert!(validate_username("alice-bob").is_ok());
    assert!(validate_username("alice_bob").is_ok());

    assert!(validate_username("alice+work").is_ok());

    // Invalid usernames
    assert!(validate_username("").is_err());
    assert!(validate_username("alice@bob").is_err());
    assert!(validate_username("alice/bob").is_err());
    assert!(validate_username("alice bob").is_err());
    assert!(validate_username("alice\tbob").is_err());
    // Outside the account profile: never lowercased or folded.
    assert!(validate_username("Alice").is_err());
    assert!(validate_username("äda").is_err());
    assert!(validate_username(&"a".repeat(65)).is_err());
}

/// A new account is named exactly in the account profile, and SCRAM lookup
/// takes the username exactly: a spelling preparation would change names no
/// account instead of the account its folded form keys.
#[tokio::test]
async fn test_names_resolve_only_in_their_canonical_spelling() {
    let db = create_test_db().await;
    let actor = crate::db::actor::DbActor::spawn(crate::db::actor::DbActor::new((*db).clone()));
    let store = NativeUserStore::new(actor);
    let register = |username: &str| RegisterRequest {
        username: username.to_string(),
        domain: "example.com".to_string(),
        password: test_password(),
        email: None,
    };

    store.register(register("ada")).await.expect("register ada");
    for refused in ["Ada", "äda", "no:pe"] {
        assert!(
            matches!(
                store.register(register(refused)).await,
                Err(AuthError::InvalidUsername(_))
            ),
            "{refused}"
        );
    }
    let duplicate = store.register(register("ada")).await;
    assert!(matches!(duplicate, Err(AuthError::UserAlreadyExists(_))));
    assert!(store.user_exists("ada", "Example.com").await.unwrap());
    assert!(store
        .get_scram_credentials("ada", "example.com")
        .await
        .unwrap()
        .is_some());
    assert!(store
        .get_scram_credentials("ADA", "example.com")
        .await
        .unwrap()
        .is_none());
}

/// A node predating the lookup keys writes `Bob` without one. Until the
/// backfill keys it, registering `bob` here is refused instead of taking
/// the JID the backfill would then leave `Bob` without.
#[tokio::test]
async fn test_unkeyed_account_blocks_a_case_variant() {
    let db = create_test_db().await;
    let actor = crate::db::actor::DbActor::spawn(crate::db::actor::DbActor::new((*db).clone()));
    actor
        .ask(DbExecute {
            sql: "INSERT INTO native_users (username, domain, password_hash, salt, stored_key, server_key) \
                  VALUES ('Bob', 'example.com', 'hash', 'salt', '', '')"
                .to_string(),
            params: vec![],
        })
        .await
        .expect("unkeyed account");
    let store = NativeUserStore::new(actor);

    assert!(store.user_exists("bob", "example.com").await.unwrap());
    let variant = store
        .register(RegisterRequest {
            username: "bob".to_string(),
            domain: "example.com".to_string(),
            password: test_password(),
            email: None,
        })
        .await;
    assert!(matches!(variant, Err(AuthError::UserAlreadyExists(_))));
    assert!(!store.user_exists("carol", "example.com").await.unwrap());
}
