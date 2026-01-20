use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use serde_json::json;
use std::{net::SocketAddr, sync::Arc};
use tower_http::{
    compression::CompressionLayer,
    cors::CorsLayer,
    trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer},
};
use tracing::{info, Level};

mod routes;

/// Server application state
#[derive(Clone)]
pub struct AppState {
    // TODO: Add database pool, actor system, etc.
}

impl AppState {
    pub fn new() -> Self {
        Self {}
    }
}

/// Start the HTTP server
pub async fn start() -> Result<()> {
    let state = Arc::new(AppState::new());

    let app = create_router(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    info!("Starting Axum HTTP server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Create the Axum router with all routes and middleware
fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/api/v1/health", get(health_handler))
        // Future routes will be mounted here
        // .merge(routes::auth::router())
        // .merge(routes::waddles::router())
        // .merge(routes::channels::router())
        // .merge(routes::messages::router())
        .with_state(state)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive()) // TODO: Configure proper CORS in production
}

/// Health check endpoint
async fn health_handler(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "status": "healthy",
            "service": "waddle-server",
            "version": env!("CARGO_PKG_VERSION"),
            "license": "AGPL-3.0"
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_health_endpoint() {
        let state = Arc::new(AppState::new());
        let app = create_router(state);

        let response = app
            .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
