use super::*;
use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
use crate::permissions::PermissionActor;
use crate::server::AppStateDeps;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use kameo::actor::Spawn;
use std::str::FromStr;
use tower::ServiceExt;
use waddle_xmpp::muc::room_registry_actor::RoomRegistryActor;
use waddle_xmpp::pubsub::InMemoryPubSubStorage;
use waddle_xmpp::xep::xep0421::OccupantIdSecret;

async fn create_test_upload_state() -> (Arc<UploadState>, std::path::PathBuf) {
    let config = DatabaseConfig::default();
    let pool_config = PoolConfig;
    let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

    // Run migrations
    let runner = MigrationRunner::global();
    runner.run(db_pool.global()).await.unwrap();

    // Create temp dir for local blob storage
    let upload_dir = std::env::temp_dir().join(format!("waddle-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&upload_dir).unwrap();

    let blob_storage: Arc<dyn crate::storage::BlobStorage> =
        Arc::new(crate::storage::LocalStorage::new(upload_dir.clone()));

    let db_pool = Arc::new(db_pool);
    let permission_actor = PermissionActor::spawn(PermissionActor::new_for_tests(Arc::new(
        db_pool.global().clone(),
    )));
    let muc_domain = jid::DomainPart::from_str("muc.example.com").expect("muc domain parses");
    let occupant_id_secret =
        OccupantIdSecret::new(b"test-occupant-id-secret-32-bytes-long".to_vec())
            .expect("test occupant-id secret meets length floor");
    let room_registry = RoomRegistryActor::spawn(RoomRegistryActor::new(
        muc_domain.to_string(),
        occupant_id_secret.clone(),
    ));
    let (room_serving, _room_serving_closer) =
        crate::server::room_serving_quiescence::RoomServingQuiescence::create();
    let app_state = Arc::new(AppState::new_with_deps(AppStateDeps {
        db_pool: Arc::clone(&db_pool),
        blob_storage,
        inbox_storage: Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new()),
        spaces_metadata_store: Arc::new(crate::spaces_metadata::InMemorySpacesMetadataStore::new()),
        channel_space_link_store: Arc::new(
            crate::channel_space_links::InMemoryChannelSpaceLinkStore::new(),
        ),
        pubsub_storage: Arc::new(InMemoryPubSubStorage::new()),
        room_registry,
        spaces_jid: "spaces.example.com".parse().expect("spaces JID parses"),
        muc_domain,
        occupant_id_secret,
        permission_actor,
        server_owner_jids: Arc::from(Vec::<jid::BareJid>::new()),
        clustering_readiness: crate::clustering::ClusteringReadiness::new(),
        clustering_claims: crate::clustering::ClusteringHandles::default(),
        room_serving,
    }));

    (Arc::new(UploadState::new(app_state)), upload_dir)
}

async fn create_test_slot(
    state: &UploadState,
    slot_id: &str,
    filename: &str,
    size: i64,
    content_type: &str,
) {
    let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(15)).to_rfc3339();
    let db = state.global_db();

    let query = r#"
        INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, expires_at)
        VALUES (?, 'test@example.com', ?, ?, ?, 'pending', ?)
    "#;

    let conn = db.guard().await.unwrap();
    conn.execute(
        query,
        crate::db_params![slot_id, filename, size, content_type, expires_at],
    )
    .await
    .unwrap();
}

async fn create_expired_slot(state: &UploadState, slot_id: &str) {
    // Create a slot that expired 1 hour ago
    let expires_at = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let db = state.global_db();

    let query = r#"
        INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, expires_at)
        VALUES (?, 'test@example.com', 'expired.txt', 100, 'text/plain', 'pending', ?)
    "#;

    let conn = db.guard().await.unwrap();
    conn.execute(query, crate::db_params![slot_id, expires_at])
        .await
        .unwrap();
}

#[tokio::test]
async fn link_preview_media_has_no_dedicated_http_route() {
    let (state, upload_dir) = create_test_upload_state().await;
    let app = router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/link-preview-media/sha256/86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    std::fs::remove_dir_all(upload_dir).ok();
}

#[test]
fn upload_size_to_i64_rejects_values_outside_database_range() {
    assert_eq!(upload_size_to_i64(i64::MAX as u64), Some(i64::MAX));
    assert_eq!(upload_size_to_i64(i64::MAX as u64 + 1), None);
}

#[test]
fn max_upload_size_is_capped_to_database_range() {
    let configured = (i64::MAX as u64).saturating_add(1).to_string();
    assert_eq!(
        max_upload_size_from_env_value(Some(&configured)),
        MAX_DATABASE_UPLOAD_SIZE
    );
    assert_eq!(
        max_upload_size_from_env_value(Some("not-a-number")),
        DEFAULT_MAX_UPLOAD_SIZE
    );
}

#[tokio::test]
async fn test_upload_to_valid_slot() {
    let (state, upload_dir) = create_test_upload_state().await;
    let slot_id = uuid::Uuid::new_v4().to_string();
    let file_content = b"Hello, World!";

    create_test_slot(
        &state,
        &slot_id,
        "test.txt",
        file_content.len() as i64,
        "text/plain",
    )
    .await;

    let app = router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/upload/{}", slot_id))
                .header("Content-Type", "text/plain")
                .header("Content-Length", file_content.len().to_string())
                .body(Body::from(file_content.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    // Verify file was written
    let file_path = upload_dir.join(&slot_id).join("test.txt");
    assert!(file_path.exists());

    let saved_content = std::fs::read(&file_path).unwrap();
    assert_eq!(saved_content, file_content);

    // Verify slot status was updated
    let slot = get_upload_slot(state.global_actor(), &slot_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(slot.status, "uploaded");
    assert!(slot.storage_key.is_some());

    // Cleanup
    std::fs::remove_dir_all(&upload_dir).ok();
}

#[tokio::test]
async fn test_upload_options_cors_headers() {
    let (state, upload_dir) = create_test_upload_state().await;
    let slot_id = uuid::Uuid::new_v4().to_string();
    let app = router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri(format!("/api/upload/{}", slot_id))
                .header("Origin", "https://compliance.conversations.im")
                .header("Access-Control-Request-Method", "PUT")
                .header("Access-Control-Request-Headers", "content-type")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap()
            .to_str()
            .unwrap(),
        "*"
    );
    assert!(response
        .headers()
        .contains_key("access-control-allow-methods"));
    assert!(response
        .headers()
        .contains_key("access-control-allow-headers"));

    std::fs::remove_dir_all(&upload_dir).ok();
}

#[tokio::test]
async fn test_upload_to_nonexistent_slot() {
    let (state, upload_dir) = create_test_upload_state().await;
    let app = router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/upload/nonexistent-slot")
                .header("Content-Type", "text/plain")
                .header("Content-Length", "5")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "slot_not_found");

    // Cleanup
    std::fs::remove_dir_all(&upload_dir).ok();
}

#[tokio::test]
async fn test_upload_to_expired_slot() {
    let (state, upload_dir) = create_test_upload_state().await;
    let slot_id = uuid::Uuid::new_v4().to_string();

    create_expired_slot(&state, &slot_id).await;

    let app = router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/upload/{}", slot_id))
                .header("Content-Type", "text/plain")
                .header("Content-Length", "5")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::GONE);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "slot_expired");

    // Cleanup
    std::fs::remove_dir_all(&upload_dir).ok();
}

#[tokio::test]
async fn test_upload_size_mismatch() {
    let (state, upload_dir) = create_test_upload_state().await;
    let slot_id = uuid::Uuid::new_v4().to_string();

    // Create slot expecting 100 bytes
    create_test_slot(&state, &slot_id, "test.txt", 100, "text/plain").await;

    let app = router(state.clone());

    // Try to upload only 5 bytes
    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/upload/{}", slot_id))
                .header("Content-Type", "text/plain")
                .header("Content-Length", "5")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "size_mismatch");

    // Cleanup
    std::fs::remove_dir_all(&upload_dir).ok();
}

#[tokio::test]
async fn test_upload_above_axum_default_limit_succeeds() {
    let (state, upload_dir) = create_test_upload_state().await;
    let slot_id = uuid::Uuid::new_v4().to_string();
    let file_content = vec![b'a'; 4 * 1024 * 1024];

    create_test_slot(
        &state,
        &slot_id,
        "large.bin",
        file_content.len() as i64,
        "application/octet-stream",
    )
    .await;

    let app = router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/upload/{}", slot_id))
                .header("Content-Type", "application/octet-stream")
                .header("Content-Length", file_content.len().to_string())
                .body(Body::from(file_content))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    let slot = get_upload_slot(state.global_actor(), &slot_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(slot.status, "uploaded");

    std::fs::remove_dir_all(&upload_dir).ok();
}

#[tokio::test]
async fn test_download_uploaded_file() {
    let (state, upload_dir) = create_test_upload_state().await;
    let slot_id = uuid::Uuid::new_v4().to_string();
    let file_content = b"Test file content";

    // Create and upload a file
    create_test_slot(
        &state,
        &slot_id,
        "download-test.txt",
        file_content.len() as i64,
        "text/plain",
    )
    .await;

    let app = router(state.clone());

    // Upload the file first
    let upload_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/upload/{}", slot_id))
                .header("Content-Type", "text/plain")
                .header("Content-Length", file_content.len().to_string())
                .body(Body::from(file_content.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(upload_response.status(), StatusCode::CREATED);

    // Now download it
    let download_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/files/{}/download-test.txt", slot_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(download_response.status(), StatusCode::OK);

    // Check headers
    let headers = download_response.headers();
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "text/plain"
    );

    // Check content
    let body = download_response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(body.as_ref(), file_content);

    // Cleanup
    std::fs::remove_dir_all(&upload_dir).ok();
}

#[tokio::test]
async fn test_download_nonexistent_file() {
    let (state, upload_dir) = create_test_upload_state().await;
    let app = router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/files/nonexistent/file.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Cleanup
    std::fs::remove_dir_all(&upload_dir).ok();
}

#[tokio::test]
async fn test_download_pending_slot() {
    let (state, upload_dir) = create_test_upload_state().await;
    let slot_id = uuid::Uuid::new_v4().to_string();

    // Create a slot but don't upload to it
    create_test_slot(&state, &slot_id, "pending.txt", 100, "text/plain").await;

    let app = router(state.clone());

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/files/{}/pending.txt", slot_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "file_not_found");
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("not yet uploaded"));

    // Cleanup
    std::fs::remove_dir_all(&upload_dir).ok();
}

#[tokio::test]
async fn test_double_upload_fails() {
    let (state, upload_dir) = create_test_upload_state().await;
    let slot_id = uuid::Uuid::new_v4().to_string();
    let file_content = b"Hello, World!";

    create_test_slot(
        &state,
        &slot_id,
        "test.txt",
        file_content.len() as i64,
        "text/plain",
    )
    .await;

    let app = router(state.clone());

    // First upload should succeed
    let response1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/upload/{}", slot_id))
                .header("Content-Type", "text/plain")
                .header("Content-Length", file_content.len().to_string())
                .body(Body::from(file_content.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response1.status(), StatusCode::CREATED);

    // Second upload should fail
    let response2 = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/upload/{}", slot_id))
                .header("Content-Type", "text/plain")
                .header("Content-Length", file_content.len().to_string())
                .body(Body::from(file_content.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response2.status(), StatusCode::CONFLICT);

    let body = response2.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "slot_already_used");

    // Cleanup
    std::fs::remove_dir_all(&upload_dir).ok();
}
