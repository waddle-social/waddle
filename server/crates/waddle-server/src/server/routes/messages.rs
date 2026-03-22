//! Channel message history routes.
//!
//! Provides HTTP endpoints for reading persisted channel messages:
//! - GET /v1/waddles/:waddle_id/channels/:channel_id/messages

use super::channels::{get_channel_from_db, ChannelState};
use crate::auth::AuthError;
use crate::db::Database;
use crate::messages::MessageRepository;
use crate::permissions::{Object, ObjectType, PermissionError, Subject};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use tracing::{debug, error, instrument, warn};

/// Create the channel message router.
pub fn router(channel_state: Arc<ChannelState>) -> Router {
    Router::new()
        .route(
            "/v1/waddles/:waddle_id/channels/:channel_id/messages",
            get(list_channel_messages_handler),
        )
        .with_state(channel_state)
}

#[derive(Debug, Deserialize)]
pub struct ChannelPath {
    pub waddle_id: String,
    pub channel_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ListMessagesQuery {
    pub session_id: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub before: Option<String>,
}

fn default_limit() -> usize {
    50
}

#[derive(Debug, Serialize, Clone)]
pub struct MessageAuthorResponse {
    pub user_id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub id: String,
    pub channel_id: String,
    pub author: MessageAuthorResponse,
    pub content: Option<String>,
    pub reply_to_id: Option<String>,
    pub thread_id: Option<String>,
    pub edited_at: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListMessagesResponse {
    pub messages: Vec<MessageResponse>,
    pub next_cursor: Option<String>,
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

#[derive(Debug)]
enum MessageRouteError {
    Auth(AuthError),
    Permission(PermissionError),
    NotFound(String),
    Database(String),
}

impl From<AuthError> for MessageRouteError {
    fn from(err: AuthError) -> Self {
        Self::Auth(err)
    }
}

impl From<PermissionError> for MessageRouteError {
    fn from(err: PermissionError) -> Self {
        Self::Permission(err)
    }
}

fn message_error_to_response(err: MessageRouteError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        MessageRouteError::Auth(auth_err) => {
            let (status, error_code) = match &auth_err {
                AuthError::SessionNotFound(_) => (StatusCode::NOT_FOUND, "session_not_found"),
                AuthError::SessionExpired => (StatusCode::UNAUTHORIZED, "session_expired"),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "auth_error"),
            };
            (
                status,
                Json(ErrorResponse::new(error_code, &auth_err.to_string())),
            )
        }
        MessageRouteError::Permission(perm_err) => {
            let (status, error_code) = match &perm_err {
                PermissionError::Denied(_) => (StatusCode::FORBIDDEN, "permission_denied"),
                _ => (StatusCode::BAD_REQUEST, "permission_error"),
            };
            (
                status,
                Json(ErrorResponse::new(error_code, &perm_err.to_string())),
            )
        }
        MessageRouteError::NotFound(msg) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new("not_found", &msg)),
        ),
        MessageRouteError::Database(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new("database_error", &msg)),
        ),
    }
}

/// GET /v1/waddles/:waddle_id/channels/:channel_id/messages
///
/// List persisted messages for a channel. Requires view permission on the channel.
#[instrument(skip(state))]
pub async fn list_channel_messages_handler(
    State(state): State<Arc<ChannelState>>,
    Path(path): Path<ChannelPath>,
    Query(params): Query<ListMessagesQuery>,
) -> impl IntoResponse {
    debug!(
        waddle_id = %path.waddle_id,
        channel_id = %path.channel_id,
        "Listing channel messages"
    );

    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return message_error_to_response(MessageRouteError::Auth(err)).into_response();
        }
    };

    let waddle_db = match state.app_state.db_pool.get_waddle_db(&path.waddle_id).await {
        Ok(db) => db,
        Err(err) => {
            error!("Failed to get waddle database: {}", err);
            return message_error_to_response(MessageRouteError::Database(format!(
                "Failed to access waddle database: {}",
                err
            )))
            .into_response();
        }
    };

    match get_channel_from_db(&waddle_db, &path.waddle_id, &path.channel_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return message_error_to_response(MessageRouteError::NotFound(format!(
                "Channel '{}' not found",
                path.channel_id
            )))
            .into_response();
        }
        Err(err) => {
            error!("Failed to get channel: {}", err);
            return message_error_to_response(MessageRouteError::Database(err)).into_response();
        }
    }

    let subject = Subject::user(&session.user_id);
    let channel_object = Object::new(ObjectType::Channel, &path.channel_id);
    let can_view = state
        .permission_service
        .check(&subject, "view", &channel_object)
        .await
        .map(|response| response.allowed)
        .unwrap_or(false);

    if !can_view {
        return message_error_to_response(MessageRouteError::Permission(PermissionError::Denied(
            "You do not have permission to view this channel".to_string(),
        )))
        .into_response();
    }

    let limit = params.limit.clamp(1, 200);
    let repository = MessageRepository::new(Arc::new(waddle_db.clone()));
    let (messages, next_cursor) = match repository
        .get_by_channel(&path.channel_id, limit, params.before.as_deref())
        .await
    {
        Ok(result) => result,
        Err(err) => {
            error!("Failed to list messages: {}", err);
            return message_error_to_response(MessageRouteError::Database(err.to_string()))
                .into_response();
        }
    };

    let author_ids: Vec<String> = messages
        .iter()
        .map(|message| message.author_user_id.clone())
        .collect();
    let authors = match load_authors(state.app_state.db_pool.global(), &author_ids).await {
        Ok(authors) => authors,
        Err(err) => {
            error!("Failed to load message authors: {}", err);
            return message_error_to_response(MessageRouteError::Database(err)).into_response();
        }
    };

    let response_messages = messages
        .into_iter()
        .map(|message| MessageResponse {
            id: message.id,
            channel_id: message.channel_id,
            author: authors
                .get(&message.author_user_id)
                .cloned()
                .unwrap_or_else(|| MessageAuthorResponse {
                    user_id: message.author_user_id.clone(),
                    username: "unknown".to_string(),
                    display_name: None,
                    avatar_url: None,
                }),
            content: message.content,
            reply_to_id: message.reply_to_id,
            thread_id: message.thread_id,
            edited_at: message.edited_at.map(|value| value.to_rfc3339()),
            created_at: message.created_at.to_rfc3339(),
            expires_at: message.expires_at.map(|value| value.to_rfc3339()),
        })
        .collect();

    (
        StatusCode::OK,
        Json(ListMessagesResponse {
            messages: response_messages,
            next_cursor,
        }),
    )
        .into_response()
}

async fn load_authors(
    db: &Database,
    user_ids: &[String],
) -> Result<HashMap<String, MessageAuthorResponse>, String> {
    if user_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut unique_ids = Vec::new();
    for user_id in user_ids {
        if !unique_ids.contains(user_id) {
            unique_ids.push(user_id.clone());
        }
    }

    let placeholders = std::iter::repeat_n("?", unique_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "SELECT id, username, display_name, avatar_url FROM users WHERE id IN ({})",
        placeholders
    );
    let params: Vec<libsql::Value> = unique_ids.into_iter().map(Into::into).collect();

    let mut authors = HashMap::new();
    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        let mut rows = conn
            .query(&query, params)
            .await
            .map_err(|e| format!("Failed to query authors: {}", e))?;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read author row: {}", e))?
        {
            let author = parse_author_row(&row)?;
            authors.insert(author.user_id.clone(), author);
        }
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;
        let mut rows = conn
            .query(&query, params)
            .await
            .map_err(|e| format!("Failed to query authors: {}", e))?;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read author row: {}", e))?
        {
            let author = parse_author_row(&row)?;
            authors.insert(author.user_id.clone(), author);
        }
    }

    Ok(authors)
}

fn parse_author_row(row: &libsql::Row) -> Result<MessageAuthorResponse, String> {
    Ok(MessageAuthorResponse {
        user_id: row
            .get(0)
            .map_err(|e| format!("Failed to get user_id: {}", e))?,
        username: row
            .get(1)
            .map_err(|e| format!("Failed to get username: {}", e))?,
        display_name: row
            .get(2)
            .map_err(|e| format!("Failed to get display_name: {}", e))?,
        avatar_url: row
            .get(3)
            .map_err(|e| format!("Failed to get avatar_url: {}", e))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Session;
    use crate::db::{Database, DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::permissions::{Object, ObjectType, Relation, Subject, Tuple};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use uuid::Uuid;

    async fn create_test_channel_state() -> Arc<ChannelState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .unwrap();

        let app_state = Arc::new(crate::server::AppState::new(Arc::new(db_pool)));
        Arc::new(ChannelState::new(
            app_state,
            Some(b"test-encryption-key-32-bytes!!!"),
        ))
    }

    async fn create_test_session(state: &ChannelState) -> Session {
        let user_id = Uuid::new_v4().to_string();
        let username = format!("test{}", &user_id[..8]);
        let session = Session::new(&user_id, &username, &username);
        state
            .session_manager
            .create_session(&session)
            .await
            .unwrap();
        session
    }

    async fn setup_waddle_with_permissions(
        state: &ChannelState,
        session: &Session,
    ) -> (String, Database) {
        let waddle_id = Uuid::new_v4().to_string();

        for relation in ["owner", "admin", "member"] {
            let tuple = Tuple::new(
                Object::new(ObjectType::Waddle, &waddle_id),
                Relation::new(relation),
                Subject::user(&session.user_id),
            );
            state.permission_service.write_tuple(tuple).await.unwrap();
        }

        let waddle_db = state
            .app_state
            .db_pool
            .create_waddle_db(&waddle_id)
            .await
            .unwrap();
        MigrationRunner::waddle().run(&waddle_db).await.unwrap();

        (waddle_id, waddle_db)
    }

    #[tokio::test]
    async fn test_list_messages_returns_persisted_history() {
        let state = create_test_channel_state().await;
        let session = create_test_session(&state).await;
        let (waddle_id, waddle_db) = setup_waddle_with_permissions(&state, &session).await;
        let app = super::super::channels::router(state.clone()).merge(router(state.clone()));

        let create_channel_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name":"general"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = create_channel_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let create_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let channel_id = create_json["id"].as_str().unwrap().to_string();

        let repository = MessageRepository::new(Arc::new(waddle_db.clone()));
        repository
            .create(crate::messages::MessageCreate::new(
                channel_id.clone(),
                session.user_id.clone(),
                "hello world".to_string(),
            ))
            .await
            .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/waddles/{}/channels/{}/messages?session_id={}",
                        waddle_id, channel_id, session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "hello world");
        assert_eq!(messages[0]["author"]["user_id"], session.user_id);
        assert_eq!(messages[0]["author"]["username"], session.username);
    }

    #[tokio::test]
    async fn test_list_messages_requires_channel_view_permission() {
        let state = create_test_channel_state().await;
        let owner_session = create_test_session(&state).await;
        let other_session = create_test_session(&state).await;
        let (waddle_id, _waddle_db) = setup_waddle_with_permissions(&state, &owner_session).await;
        let app = super::super::channels::router(state.clone()).merge(router(state.clone()));

        let create_channel_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, owner_session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name":"general"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = create_channel_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let create_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let channel_id = create_json["id"].as_str().unwrap().to_string();

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/waddles/{}/channels/{}/messages?session_id={}",
                        waddle_id, channel_id, other_session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
