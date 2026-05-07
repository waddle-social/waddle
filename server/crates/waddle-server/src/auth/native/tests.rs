use super::*;
use crate::db::{Database, MigrationRunner};
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
    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
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
    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
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
    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
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
    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
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
    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
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
    let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
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

    // Invalid usernames
    assert!(validate_username("").is_err());
    assert!(validate_username("alice@bob").is_err());
    assert!(validate_username("alice/bob").is_err());
    assert!(validate_username("alice bob").is_err());
    assert!(validate_username("alice\tbob").is_err());
}
