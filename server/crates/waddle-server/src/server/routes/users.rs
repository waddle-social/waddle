//! User search routes for community administration.
//!
//! Provides HTTP endpoints for looking up users:
//! - GET /v1/users/search

use super::waddles::WaddleState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, instrument, warn};

/// Create the user search router.
pub fn router(state: Arc<WaddleState>) -> Router {
    Router::new()
        .route("/v1/users/search", get(search_users_handler))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
pub struct SearchUsersQuery {
    pub session_id: String,
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    10
}

#[derive(Debug, Serialize)]
pub struct SearchUserResponse {
    pub id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub jid: String,
}

#[derive(Debug, Serialize)]
pub struct SearchUsersResponse {
    pub users: Vec<SearchUserResponse>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
}

impl ErrorResponse {
    fn new(error: &str, message: &str) -> Self {
        Self {
            error: error.to_string(),
            message: message.to_string(),
        }
    }
}

/// GET /v1/users/search
///
/// Search users by username or display name prefix. Requires an authenticated session.
#[instrument(skip(state))]
pub async fn search_users_handler(
    State(state): State<Arc<WaddleState>>,
    Query(params): Query<SearchUsersQuery>,
) -> impl IntoResponse {
    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            let (status, error_code) = match err {
                crate::auth::AuthError::SessionNotFound(_) => {
                    (StatusCode::NOT_FOUND, "session_not_found")
                }
                crate::auth::AuthError::SessionExpired => {
                    (StatusCode::UNAUTHORIZED, "session_expired")
                }
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "auth_error"),
            };
            return (
                status,
                Json(ErrorResponse::new(error_code, &err.to_string())),
            )
                .into_response();
        }
    };

    let query = params.query.trim();
    if query.is_empty() {
        return (
            StatusCode::OK,
            Json(SearchUsersResponse { users: Vec::new() }),
        )
            .into_response();
    }

    let limit = params.limit.clamp(1, 20) as i64;
    let users = match search_users(
        state.app_state.db_pool.global(),
        query,
        limit,
        &std::env::var("WADDLE_XMPP_DOMAIN").unwrap_or_else(|_| "localhost".to_string()),
    )
    .await
    {
        Ok(users) => users,
        Err(err) => {
            error!("Failed to search users: {}", err);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new("database_error", &err)),
            )
                .into_response();
        }
    };

    let users = users
        .into_iter()
        .filter(|user| user.id != session.user_id)
        .collect();

    (StatusCode::OK, Json(SearchUsersResponse { users })).into_response()
}

async fn search_users(
    db: &crate::db::Database,
    query: &str,
    limit: i64,
    xmpp_domain: &str,
) -> Result<Vec<SearchUserResponse>, String> {
    let sql = r#"
        SELECT id, username, display_name, avatar_url, xmpp_localpart
        FROM users
        WHERE lower(username) LIKE lower(?1) || '%'
           OR (display_name IS NOT NULL AND lower(display_name) LIKE lower(?1) || '%')
        ORDER BY
            CASE
                WHEN lower(username) = lower(?1) THEN 0
                WHEN lower(username) LIKE lower(?1) || '%' THEN 1
                WHEN display_name IS NOT NULL AND lower(display_name) = lower(?1) THEN 2
                ELSE 3
            END,
            username ASC
        LIMIT ?2
    "#;

    let conn = db
        .guard()
        .await
        .map_err(|e| e.to_string())?;

    let mut users = Vec::new();
    let mut rows = conn
        .query(sql, libsql::params![query, limit])
        .await
        .map_err(|e| format!("Failed to query users: {}", e))?;

    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Failed to read user row: {}", e))?
    {
        users.push(parse_user_row(&row, xmpp_domain)?);
    }

    Ok(users)
}

fn parse_user_row(row: &libsql::Row, xmpp_domain: &str) -> Result<SearchUserResponse, String> {
    let id: String = row
        .get(0)
        .map_err(|e| format!("Failed to get user id: {}", e))?;
    let username: String = row
        .get(1)
        .map_err(|e| format!("Failed to get username: {}", e))?;
    let display_name: Option<String> = row
        .get(2)
        .map_err(|e| format!("Failed to get display_name: {}", e))?;
    let avatar_url: Option<String> = row
        .get(3)
        .map_err(|e| format!("Failed to get avatar_url: {}", e))?;
    let xmpp_localpart: String = row
        .get(4)
        .map_err(|e| format!("Failed to get xmpp_localpart: {}", e))?;

    Ok(SearchUserResponse {
        id,
        username,
        display_name,
        avatar_url,
        jid: format!("{xmpp_localpart}@{xmpp_domain}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Session;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use uuid::Uuid;

    async fn create_test_waddle_state() -> Arc<WaddleState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .unwrap();

        let app_state = Arc::new(crate::server::AppState::new(Arc::new(db_pool)));
        Arc::new(WaddleState::new(
            app_state,
            Some(b"test-encryption-key-32-bytes!!!"),
            false,
        ))
    }

    async fn create_test_session(state: &WaddleState, username: Option<&str>) -> Session {
        let user_id = Uuid::new_v4().to_string();
        let username = username
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("test{}", &user_id[..8]));
        let session = Session::new(&user_id, &username, &username);
        state
            .session_manager
            .create_session(&session)
            .await
            .unwrap();
        session
    }

    #[tokio::test]
    async fn test_search_users_by_prefix() {
        let state = create_test_waddle_state().await;
        let searcher = create_test_session(&state, Some("searcher")).await;
        let _alice = create_test_session(&state, Some("alice")).await;
        let _alina = create_test_session(&state, Some("alina")).await;
        let _bob = create_test_session(&state, Some("bob")).await;
        let app = router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/users/search?session_id={}&query=ali",
                        searcher.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let users = json["users"].as_array().unwrap();

        assert_eq!(users.len(), 2);
        assert_eq!(users[0]["username"], "alice");
        assert_eq!(users[1]["username"], "alina");
    }
}
