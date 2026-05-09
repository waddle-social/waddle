use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Extension, Path},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracing::{error, warn};
use waddle_extensions::{
    ExtensionEffect, PluginId, ProviderDeliveryId, ProviderEventType, ProviderField,
    ProviderFieldName, ProviderFieldNumber, ProviderFieldText, ProviderFieldValue, ProviderId,
    ProviderPayload, ProviderWebhook, WaddleId,
};

use crate::{db::actor::DbExecute, db_params};

use super::websocket::WebSocketState;

type HmacSha256 = Hmac<Sha256>;

pub fn router(websocket_state: Arc<WebSocketState>) -> Router {
    Router::new()
        .route(
            "/webhooks/providers/:provider_id/:plugin_id",
            post(provider_webhook_handler),
        )
        .layer(Extension(websocket_state))
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ProviderIngressPath {
    provider_id: String,
    plugin_id: String,
}

#[derive(Serialize)]
struct WebhookAccepted {
    accepted: bool,
    provider: String,
    plugin: String,
    event_type: String,
    delivery_id: String,
    queued: bool,
    duplicate: bool,
}

#[derive(Debug, thiserror::Error)]
enum WebhookError {
    #[error("provider webhook secret is not configured")]
    MissingSecret,
    #[error("failed to read provider webhook secret file")]
    SecretFile(#[from] std::io::Error),
    #[error("webhook payload required")]
    EmptyBody,
    #[error("missing provider event header")]
    MissingEventType,
    #[error("missing provider delivery header")]
    MissingDeliveryId,
    #[error("invalid webhook signature")]
    InvalidSignature,
    #[error("invalid provider id: {0}")]
    InvalidProviderId(String),
    #[error("invalid plugin id: {0}")]
    InvalidPluginId(String),
    #[error("invalid webhook payload: {0}")]
    InvalidPayload(String),
    #[error("extension plugin is not loaded")]
    PluginUnavailable,
    #[error("webhook delivery ledger failed: {0}")]
    Ledger(String),
}

#[derive(Debug, Clone)]
struct ProviderIngressConfig {
    secret: String,
    event_header: String,
    delivery_header: String,
    signature_header: String,
    signature_prefix: String,
}

async fn provider_webhook_handler(
    Extension(websocket_state): Extension<Arc<WebSocketState>>,
    Path(path): Path<ProviderIngressPath>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    match accept_provider_webhook(websocket_state, path, &headers, body.as_ref()).await {
        Ok(accepted) => (StatusCode::ACCEPTED, Json(accepted)).into_response(),
        Err(error) => webhook_error_response(error),
    }
}

async fn accept_provider_webhook(
    websocket_state: Arc<WebSocketState>,
    path: ProviderIngressPath,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<WebhookAccepted, WebhookError> {
    if body.is_empty() {
        return Err(WebhookError::EmptyBody);
    }

    let provider = typed(
        ProviderId::new(path.provider_id.clone()),
        WebhookError::InvalidProviderId,
    )?;
    let plugin = typed(
        PluginId::new(path.plugin_id.clone()),
        WebhookError::InvalidPluginId,
    )?;
    let config = provider_ingress_config(provider.as_str())?;
    if !verify_hmac_sha256_signature(
        headers,
        body,
        config.secret.as_bytes(),
        &config.signature_header,
        &config.signature_prefix,
    ) {
        return Err(WebhookError::InvalidSignature);
    }

    let event_type = provider_header(headers, &config.event_header)
        .ok_or(WebhookError::MissingEventType)?
        .to_string();
    let delivery_id = provider_header(headers, &config.delivery_header)
        .ok_or(WebhookError::MissingDeliveryId)?
        .to_string();
    let event_type = typed(ProviderEventType::new(event_type), |error| {
        WebhookError::InvalidPayload(error.to_string())
    })?;
    let delivery_id = typed(ProviderDeliveryId::new(delivery_id), |error| {
        WebhookError::InvalidPayload(error.to_string())
    })?;
    let payload = parse_provider_payload(body)?;

    if !websocket_state
        .deps
        .protocol
        .extension_manager
        .has_plugin(plugin.as_str())
    {
        return Err(WebhookError::PluginUnavailable);
    }

    let event = ProviderWebhook {
        waddle_id: typed(
            WaddleId::new(format!(
                "provider-{}-delivery-{}",
                provider.as_str(),
                delivery_id.as_str()
            )),
            |error| WebhookError::InvalidPayload(error.to_string()),
        )?,
        provider: provider.clone(),
        event_type: event_type.clone(),
        delivery_id: delivery_id.clone(),
        payload,
    };
    let payload_sha256 = hex::encode(Sha256::digest(body));
    let inserted = record_provider_delivery(
        &websocket_state,
        plugin.as_str(),
        &event,
        payload_sha256.as_str(),
    )
    .await?;
    if !inserted {
        return Ok(WebhookAccepted {
            accepted: true,
            provider: provider.into_string(),
            plugin: plugin.into_string(),
            event_type: event_type.into_string(),
            delivery_id: delivery_id.into_string(),
            queued: false,
            duplicate: true,
        });
    }

    let dispatch_state = Arc::clone(&websocket_state);
    let dispatch_plugin = plugin.clone();
    let dispatch_provider = provider.clone();
    let dispatch_delivery = delivery_id.clone();
    tokio::spawn(async move {
        dispatch_provider_delivery(
            dispatch_state,
            dispatch_plugin,
            dispatch_provider,
            dispatch_delivery,
            event,
        )
        .await;
    });

    Ok(WebhookAccepted {
        accepted: true,
        provider: provider.into_string(),
        plugin: plugin.into_string(),
        event_type: event_type.into_string(),
        delivery_id: delivery_id.into_string(),
        queued: true,
        duplicate: false,
    })
}

async fn dispatch_provider_delivery(
    websocket_state: Arc<WebSocketState>,
    plugin: PluginId,
    provider: ProviderId,
    delivery_id: ProviderDeliveryId,
    event: ProviderWebhook,
) {
    let effects = websocket_state
        .deps
        .protocol
        .extension_manager
        .invoke_provider_webhook(plugin.as_str(), event)
        .await;
    let (status, error_text) = if webhook_effects_failed(&effects) {
        (
            "failed",
            Some("extension provider webhook returned a host warning"),
        )
    } else {
        ("processed", None)
    };
    if let Err(error) = mark_provider_delivery(
        &websocket_state,
        provider.as_str(),
        delivery_id.as_str(),
        status,
        error_text,
    )
    .await
    {
        error!(
            provider = %provider,
            delivery_id = %delivery_id,
            error = %error,
            "failed to update provider webhook delivery ledger"
        );
    }
}

async fn record_provider_delivery(
    websocket_state: &WebSocketState,
    plugin_id: &str,
    event: &ProviderWebhook,
    payload_sha256: &str,
) -> Result<bool, WebhookError> {
    let rows = websocket_state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: r#"
                INSERT INTO provider_webhook_deliveries (
                    provider_id,
                    delivery_id,
                    plugin_id,
                    event_type,
                    payload_sha256,
                    status,
                    attempts
                )
                VALUES (?, ?, ?, ?, ?, 'queued', 0)
                ON CONFLICT(provider_id, delivery_id) DO NOTHING
            "#
            .to_string(),
            params: db_params![
                event.provider.as_str(),
                event.delivery_id.as_str(),
                plugin_id,
                event.event_type.as_str(),
                payload_sha256,
            ],
        })
        .await
        .map_err(|error| WebhookError::Ledger(error.to_string()))?;
    Ok(rows > 0)
}

async fn mark_provider_delivery(
    websocket_state: &WebSocketState,
    provider_id: &str,
    delivery_id: &str,
    status: &str,
    last_error: Option<&str>,
) -> Result<(), WebhookError> {
    websocket_state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .ask(DbExecute {
            sql: r#"
                UPDATE provider_webhook_deliveries
                SET status = ?,
                    attempts = attempts + 1,
                    last_error = ?,
                    updated_at = CURRENT_TIMESTAMP
                WHERE provider_id = ? AND delivery_id = ?
            "#
            .to_string(),
            params: db_params![status, last_error, provider_id, delivery_id],
        })
        .await
        .map_err(|error| WebhookError::Ledger(error.to_string()))?;
    Ok(())
}

fn provider_ingress_config(provider_id: &str) -> Result<ProviderIngressConfig, WebhookError> {
    let key = provider_env_key(provider_id);
    let secret_env = format!("WADDLE_PROVIDER_{key}_WEBHOOK_SECRET");
    let secret_file_env = format!("WADDLE_PROVIDER_{key}_WEBHOOK_SECRET_FILE");
    let event_header_env = format!("WADDLE_PROVIDER_{key}_WEBHOOK_EVENT_HEADER");
    let delivery_header_env = format!("WADDLE_PROVIDER_{key}_WEBHOOK_DELIVERY_HEADER");
    let signature_header_env = format!("WADDLE_PROVIDER_{key}_WEBHOOK_SIGNATURE_HEADER");
    let signature_prefix_env = format!("WADDLE_PROVIDER_{key}_WEBHOOK_SIGNATURE_PREFIX");

    let secret = std::env::var(&secret_env)
        .ok()
        .map(|secret| secret.trim().to_string())
        .filter(|secret| !secret.is_empty())
        .map(Ok)
        .unwrap_or_else(|| {
            let path = std::env::var(&secret_file_env)
                .ok()
                .map(|path| path.trim().to_string())
                .filter(|path| !path.is_empty())
                .ok_or(WebhookError::MissingSecret)?;
            std::fs::read_to_string(path)
                .map(|secret| secret.trim().to_string())
                .map_err(WebhookError::SecretFile)
        })?;
    if secret.is_empty() {
        return Err(WebhookError::MissingSecret);
    }

    Ok(ProviderIngressConfig {
        secret,
        event_header: std::env::var(event_header_env)
            .unwrap_or_else(|_| "x-provider-event".to_string()),
        delivery_header: std::env::var(delivery_header_env)
            .unwrap_or_else(|_| "x-provider-delivery".to_string()),
        signature_header: std::env::var(signature_header_env)
            .unwrap_or_else(|_| "x-provider-signature-256".to_string()),
        signature_prefix: std::env::var(signature_prefix_env).unwrap_or_else(|_| "sha256=".into()),
    })
}

fn provider_env_key(provider_id: &str) -> String {
    provider_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn webhook_error_response(error: WebhookError) -> axum::response::Response {
    let status = match error {
        WebhookError::MissingSecret | WebhookError::SecretFile(_) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        WebhookError::PluginUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        WebhookError::InvalidSignature => StatusCode::UNAUTHORIZED,
        WebhookError::Ledger(_) => StatusCode::SERVICE_UNAVAILABLE,
        WebhookError::EmptyBody
        | WebhookError::MissingEventType
        | WebhookError::MissingDeliveryId
        | WebhookError::InvalidProviderId(_)
        | WebhookError::InvalidPluginId(_)
        | WebhookError::InvalidPayload(_) => StatusCode::BAD_REQUEST,
    };
    (status, error.to_string()).into_response()
}

fn webhook_effects_failed(effects: &[ExtensionEffect]) -> bool {
    effects
        .iter()
        .any(|effect| matches!(effect, ExtensionEffect::HostWarning(_)))
}

fn provider_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn verify_hmac_sha256_signature(
    headers: &HeaderMap,
    payload: &[u8],
    secret: &[u8],
    signature_header: &str,
    signature_prefix: &str,
) -> bool {
    let Some(sig_header) = provider_header(headers, signature_header) else {
        return false;
    };
    let Some(given_hex) = sig_header.strip_prefix(signature_prefix) else {
        return false;
    };
    let Ok(given) = hex::decode(given_hex) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return false;
    };
    mac.update(payload);
    mac.verify_slice(&given).is_ok()
}

fn parse_provider_payload(payload: &[u8]) -> Result<ProviderPayload, WebhookError> {
    let json = serde_json::from_slice::<serde_json::Value>(payload)
        .map_err(|error| WebhookError::InvalidPayload(error.to_string()))?;
    let mut fields = Vec::new();
    flatten_json_payload(&json, &mut Vec::new(), &mut fields)?;
    Ok(ProviderPayload { fields })
}

fn flatten_json_payload(
    value: &serde_json::Value,
    path: &mut Vec<ProviderFieldName>,
    fields: &mut Vec<ProviderField>,
) -> Result<(), WebhookError> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let segment = ProviderFieldName::new(key.clone())
                    .map_err(|error| WebhookError::InvalidPayload(error.to_string()))?;
                path.push(segment);
                flatten_json_payload(value, path, fields)?;
                path.pop();
            }
        }
        serde_json::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                let segment = ProviderFieldName::new(index.to_string())
                    .map_err(|error| WebhookError::InvalidPayload(error.to_string()))?;
                path.push(segment);
                flatten_json_payload(value, path, fields)?;
                path.pop();
            }
        }
        _ => {
            if path.is_empty() {
                return Err(WebhookError::InvalidPayload(
                    "provider payload root must be an object or array".to_string(),
                ));
            }
            fields.push(ProviderField {
                path: path.clone(),
                value: provider_field_value(value)?,
            });
        }
    }
    Ok(())
}

fn provider_field_value(value: &serde_json::Value) -> Result<ProviderFieldValue, WebhookError> {
    match value {
        serde_json::Value::Null => Ok(ProviderFieldValue::Null),
        serde_json::Value::Bool(value) => Ok(ProviderFieldValue::Boolean(*value)),
        serde_json::Value::Number(value) => ProviderFieldNumber::new(value.to_string())
            .map(ProviderFieldValue::Number)
            .map_err(|error| WebhookError::InvalidPayload(error.to_string())),
        serde_json::Value::String(value) => ProviderFieldText::new(value.clone())
            .map(ProviderFieldValue::Text)
            .map_err(|error| WebhookError::InvalidPayload(error.to_string())),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            warn!("nested provider payload container reached leaf conversion");
            Err(WebhookError::InvalidPayload(
                "nested container leaf was not flattened".to_string(),
            ))
        }
    }
}

fn typed<T, E, F>(value: Result<T, E>, error: F) -> Result<T, WebhookError>
where
    E: std::fmt::Display,
    F: FnOnce(String) -> WebhookError,
{
    value.map_err(|error_text| error(error_text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_configured_hmac_sha256_signature() {
        let payload = b"{\"zen\":\"typed events\"}";
        let secret = b"secret";
        let mut mac = HmacSha256::new_from_slice(secret).expect("hmac");
        mac.update(payload);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-provider-signature-256",
            signature.parse().expect("signature header"),
        );

        assert!(verify_hmac_sha256_signature(
            &headers,
            payload,
            secret,
            "x-provider-signature-256",
            "sha256=",
        ));
        assert!(!verify_hmac_sha256_signature(
            &headers,
            b"{}",
            secret,
            "x-provider-signature-256",
            "sha256=",
        ));
    }

    #[test]
    fn flattens_provider_json_payload_without_provider_semantics() {
        let payload = br#"{
            "action": "completed",
            "subject": {"id": 100, "name": "example"},
            "event": {"status": "failure", "attempt": 2}
        }"#;

        let parsed = parse_provider_payload(payload).expect("payload");
        let fields = parsed
            .fields
            .iter()
            .map(|field| {
                let path = field
                    .path
                    .iter()
                    .map(|segment| segment.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                (path, &field.value)
            })
            .collect::<Vec<_>>();

        assert!(fields.iter().any(|(path, value)| {
            path == "action"
                && matches!(value, ProviderFieldValue::Text(text) if text.as_str() == "completed")
        }));
        assert!(fields.iter().any(|(path, value)| {
            path == "subject.id"
                && matches!(value, ProviderFieldValue::Number(number) if number.as_str() == "100")
        }));
        assert!(fields.iter().any(|(path, value)| {
            path == "event.status"
                && matches!(value, ProviderFieldValue::Text(text) if text.as_str() == "failure")
        }));
    }

    #[test]
    fn host_warning_effect_marks_webhook_dispatch_failed() {
        let warning = ExtensionEffect::HostWarning(
            waddle_extensions::DisplayText::new("extension failed").expect("warning text"),
        );

        assert!(webhook_effects_failed(&[warning]));
        assert!(!webhook_effects_failed(&[ExtensionEffect::Noop]));
    }
}
