use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Extension, Path},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tracing::{error, warn};
use waddle_extensions::{
    ExtensionEffect, PluginId, ProviderDeliveryId, ProviderEventType, ProviderField,
    ProviderFieldName, ProviderFieldNumber, ProviderFieldText, ProviderFieldValue, ProviderId,
    ProviderPayload, ProviderWebhook, WaddleId,
};

use crate::{db::actor::DbExecute, db_params};

use super::super::extension_commands::pubsub::publish_extension_pubsub;
use super::websocket::WebSocketState;

type HmacSha256 = Hmac<Sha256>;

pub fn router(websocket_state: Arc<WebSocketState>) -> Router {
    Router::new()
        .route(
            "/webhooks/providers/{provider_id}/{plugin_id}",
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
    #[error("provider webhook is not configured for this extension plugin")]
    PluginNotAllowed,
    #[error("webhook delivery ledger failed: {0}")]
    Ledger(String),
}

#[derive(Debug, thiserror::Error)]
pub enum IngressRegistryError {
    #[error("invalid provider ingress env var {var}: {detail}")]
    InvalidEnvVar { var: String, detail: String },
    #[error("failed to read provider webhook secret file {path}: {source}")]
    SecretFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone)]
pub struct ProviderIngressConfig {
    plugin: PluginId,
    secret: String,
    event_header: String,
    delivery_header: String,
    signature_header: String,
    signature_prefix: String,
}

/// Snapshot of `WADDLE_PROVIDER_*_WEBHOOK_*` env vars, built once at server
/// startup. The handler does an `O(1)` lookup keyed by the URL `provider_id`'s
/// env-key form (uppercased alphanumeric); no per-request env reads or file
/// I/O.
#[derive(Debug, Default)]
pub struct ProviderIngressRegistry {
    configs: HashMap<String, ProviderIngressConfig>,
}

impl ProviderIngressRegistry {
    pub fn from_env() -> Result<Self, IngressRegistryError> {
        Self::from_vars(std::env::vars())
    }

    fn from_vars<I>(vars: I) -> Result<Self, IngressRegistryError>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let env: HashMap<String, String> = vars.into_iter().collect();
        let mut configs = HashMap::new();
        for (key, value) in &env {
            let Some(provider_key) = key
                .strip_prefix("WADDLE_PROVIDER_")
                .and_then(|rest| rest.strip_suffix("_WEBHOOK_SECRET"))
            else {
                continue;
            };
            let provider_key = provider_key.to_string();
            let trimmed = value.trim();
            if trimmed.is_empty() {
                continue;
            }
            let config = build_ingress_config(&provider_key, trimmed.to_string(), &env)?;
            configs.insert(provider_key, config);
        }
        // Also pick up providers configured only via *_SECRET_FILE.
        for (key, value) in &env {
            let Some(provider_key) = key
                .strip_prefix("WADDLE_PROVIDER_")
                .and_then(|rest| rest.strip_suffix("_WEBHOOK_SECRET_FILE"))
            else {
                continue;
            };
            let provider_key = provider_key.to_string();
            if configs.contains_key(&provider_key) {
                continue;
            }
            let path = value.trim();
            if path.is_empty() {
                continue;
            }
            let secret = std::fs::read_to_string(path)
                .map_err(|source| IngressRegistryError::SecretFile {
                    path: path.to_string(),
                    source,
                })?
                .trim()
                .to_string();
            if secret.is_empty() {
                continue;
            }
            let config = build_ingress_config(&provider_key, secret, &env)?;
            configs.insert(provider_key, config);
        }
        Ok(Self { configs })
    }

    fn get(&self, provider_id: &str) -> Option<&ProviderIngressConfig> {
        self.configs.get(&provider_env_key(provider_id))
    }
}

fn build_ingress_config(
    provider_key: &str,
    secret: String,
    env: &HashMap<String, String>,
) -> Result<ProviderIngressConfig, IngressRegistryError> {
    let pick = |suffix: &str, default: &str| -> String {
        env.get(&format!("WADDLE_PROVIDER_{provider_key}_WEBHOOK_{suffix}"))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let plugin_var = format!("WADDLE_PROVIDER_{provider_key}_WEBHOOK_PLUGIN");
    let plugin_name = env
        .get(&plugin_var)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_plugin_for_provider_key(provider_key));
    let plugin =
        PluginId::new(plugin_name).map_err(|error| IngressRegistryError::InvalidEnvVar {
            var: plugin_var,
            detail: error.to_string(),
        })?;
    Ok(ProviderIngressConfig {
        plugin,
        secret,
        event_header: pick("EVENT_HEADER", "x-provider-event"),
        delivery_header: pick("DELIVERY_HEADER", "x-provider-delivery"),
        signature_header: pick("SIGNATURE_HEADER", "x-provider-signature-256"),
        signature_prefix: pick("SIGNATURE_PREFIX", "sha256="),
    })
}

fn default_plugin_for_provider_key(provider_key: &str) -> String {
    provider_key.to_ascii_lowercase().replace('_', "-")
}

/// Tracker for in-flight provider webhook dispatch tasks. Each dispatch
/// is spawned through this so graceful shutdown can `close()` + `wait()`
/// before tearing down the runtime. Note: a row inserted into
/// `provider_webhook_deliveries` with `status = 'queued'` that the dispatch
/// task never reaches (process kill, runtime drop) stays `queued` forever —
/// V1 has no sweep/retry loop. Operators should look at stuck rows.
pub type ProviderDispatchTracker = tokio_util::task::TaskTracker;

async fn provider_webhook_handler(
    Extension(websocket_state): Extension<Arc<WebSocketState>>,
    Path(path): Path<ProviderIngressPath>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let scope = match websocket_state.deps.room_serving.try_scope() {
        Ok(scope) => scope,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match accept_provider_webhook(websocket_state, path, &headers, body.as_ref(), scope).await {
        Ok(accepted) => (StatusCode::ACCEPTED, Json(accepted)).into_response(),
        Err(error) => webhook_error_response(error),
    }
}

async fn accept_provider_webhook(
    websocket_state: Arc<WebSocketState>,
    path: ProviderIngressPath,
    headers: &HeaderMap,
    body: &[u8],
    mut room_scope: crate::server::room_serving_quiescence::RoomServingScope,
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
    let config = websocket_state
        .deps
        .provider_ingress
        .get(provider.as_str())
        .ok_or(WebhookError::MissingSecret)?;
    if config.plugin != plugin {
        return Err(WebhookError::PluginNotAllowed);
    }
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
    // Validation failures above are settled and leave the admission scope
    // unarmed. From the delivery-ledger insert onward the request can enqueue
    // room-capable extension work, so cancellation/ambiguity must latch
    // terminal release unsafe.
    room_scope.arm();
    let payload_sha256 = hex::encode(Sha256::digest(body));
    let inserted = record_provider_delivery(
        &websocket_state,
        plugin.as_str(),
        &event,
        payload_sha256.as_str(),
    )
    .await?;
    if !inserted {
        room_scope.complete_clean();
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
    websocket_state
        .deps
        .provider_dispatch_tasks
        .spawn(async move {
            dispatch_provider_delivery(
                dispatch_state,
                dispatch_plugin,
                dispatch_provider,
                dispatch_delivery,
                event,
            )
            .await;
            room_scope.complete_clean();
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
    let mut error_text = if webhook_effects_failed(&effects) {
        Some("extension provider webhook returned a host warning".to_string())
    } else {
        None
    };
    let owner = match websocket_state.deps.service_domains.extensions.parse() {
        Ok(owner) => owner,
        Err(error) => {
            error_text = Some(format!("invalid extension PubSub owner JID: {error}"));
            if let Err(error) = mark_provider_delivery(
                &websocket_state,
                provider.as_str(),
                delivery_id.as_str(),
                "failed",
                error_text.as_deref(),
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
            return;
        }
    };
    if let Err(error) = publish_provider_pubsub_effects(
        websocket_state.deps.protocol.pubsub_storage.as_ref(),
        &owner,
        &effects,
    )
    .await
    {
        error!(
            provider = %provider,
            delivery_id = %delivery_id,
            error = %error,
            "failed to apply provider webhook PubSub effects"
        );
        error_text = Some(format!(
            "extension provider webhook PubSub effect failed: {error}"
        ));
    }
    let status = if error_text.is_some() {
        "failed"
    } else {
        "processed"
    };
    if let Err(error) = mark_provider_delivery(
        &websocket_state,
        provider.as_str(),
        delivery_id.as_str(),
        status,
        error_text.as_deref(),
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

async fn publish_provider_pubsub_effects(
    storage: &dyn waddle_xmpp::pubsub::PubSubStorage,
    owner: &jid::BareJid,
    effects: &[ExtensionEffect],
) -> Result<usize, String> {
    let mut published = 0;
    for effect in effects {
        if let ExtensionEffect::PublishPubSub(publish) = effect {
            publish_extension_pubsub(storage, owner, publish.clone())
                .await
                .map_err(|error| {
                    format!(
                        "failed to publish extension PubSub item to {}: {error}",
                        publish.node
                    )
                })?;
            published += 1;
        }
    }
    Ok(published)
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
        WebhookError::MissingSecret => StatusCode::SERVICE_UNAVAILABLE,
        WebhookError::PluginUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        WebhookError::PluginNotAllowed => StatusCode::FORBIDDEN,
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
            "event": {"status": "failure", "attempt": 2, "description": ""}
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
        assert!(fields.iter().any(|(path, value)| {
            path == "event.description"
                && matches!(value, ProviderFieldValue::Text(text) if text.as_str().is_empty())
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

    #[tokio::test]
    async fn provider_pubsub_effects_are_persisted() {
        use waddle_extensions::{
            ExtensionPayload, PayloadNamespace, PubSubItemId, PubSubNode, PubSubPublish, XmlElement,
        };
        use waddle_xmpp::pubsub::{InMemoryPubSubStorage, PubSubStorage};

        let storage = InMemoryPubSubStorage::new();
        let owner: jid::BareJid = "extensions.waddle.social".parse().expect("owner JID");
        let namespace =
            PayloadNamespace::new("urn:waddle:test-extension:1").expect("payload namespace");
        let payload = ExtensionPayload::new(
            namespace.clone(),
            XmlElement::new(namespace, "route", vec![], vec![]).expect("payload root"),
        )
        .expect("payload");
        let node = PubSubNode::new("urn:waddle:test-extension:1:routes").expect("node");
        let item_id = PubSubItemId::new("100").expect("item id");
        let effects = vec![ExtensionEffect::PublishPubSub(PubSubPublish {
            node: node.clone(),
            item_id: Some(item_id.clone()),
            payload,
        })];

        let published = publish_provider_pubsub_effects(&storage, &owner, &effects)
            .await
            .expect("pubsub effects persisted");

        assert_eq!(published, 1);
        let items = storage
            .get_items(&owner, node.as_str(), None, &[item_id.into_string()])
            .await
            .expect("stored items");
        assert_eq!(items.len(), 1);
        let item = items[0].to_pubsub_item();
        let payload = item.payload.as_ref().expect("stored payload");
        assert_eq!(payload.name(), "route");
        assert_eq!(payload.ns(), "urn:waddle:test-extension:1");
    }

    #[test]
    fn ingress_registry_builds_from_env_inline_secret() {
        let registry = ProviderIngressRegistry::from_vars([
            (
                "WADDLE_PROVIDER_GITHUB_WEBHOOK_SECRET".to_string(),
                "  not-a-real-secret  ".to_string(),
            ),
            (
                "WADDLE_PROVIDER_GITHUB_WEBHOOK_EVENT_HEADER".to_string(),
                "x-github-event".to_string(),
            ),
            (
                "WADDLE_PROVIDER_GITHUB_WEBHOOK_DELIVERY_HEADER".to_string(),
                "x-github-delivery".to_string(),
            ),
            (
                "WADDLE_PROVIDER_GITHUB_WEBHOOK_SIGNATURE_HEADER".to_string(),
                "x-hub-signature-256".to_string(),
            ),
            (
                "WADDLE_PROVIDER_GITHUB_WEBHOOK_SIGNATURE_PREFIX".to_string(),
                "sha256=".to_string(),
            ),
            ("UNRELATED".to_string(), "value".to_string()),
        ])
        .expect("registry build");

        let config = registry.get("github").expect("github config");
        assert_eq!(config.plugin.as_str(), "github");
        assert_eq!(config.secret, "not-a-real-secret");
        assert_eq!(config.event_header, "x-github-event");
        assert_eq!(config.delivery_header, "x-github-delivery");
        assert_eq!(config.signature_header, "x-hub-signature-256");
        assert_eq!(config.signature_prefix, "sha256=");
        assert!(registry.get("missing-provider").is_none());
    }

    #[test]
    fn ingress_registry_accepts_explicit_plugin_binding() {
        let registry = ProviderIngressRegistry::from_vars([
            (
                "WADDLE_PROVIDER_GITHUB_WEBHOOK_SECRET".to_string(),
                "not-a-real-secret".to_string(),
            ),
            (
                "WADDLE_PROVIDER_GITHUB_WEBHOOK_PLUGIN".to_string(),
                "github".to_string(),
            ),
        ])
        .expect("registry build");

        assert_eq!(
            registry
                .get("github")
                .expect("github config")
                .plugin
                .as_str(),
            "github"
        );
    }

    #[test]
    fn default_provider_plugin_replaces_env_underscores_with_plugin_dashes() {
        assert_eq!(default_plugin_for_provider_key("GITHUB_APP"), "github-app");
    }

    #[test]
    fn ingress_registry_skips_blank_secret() {
        let registry = ProviderIngressRegistry::from_vars([(
            "WADDLE_PROVIDER_GITHUB_WEBHOOK_SECRET".to_string(),
            "   ".to_string(),
        )])
        .expect("registry build");
        assert!(registry.get("github").is_none());
    }

    #[test]
    fn ingress_registry_defaults_headers_when_unset() {
        let registry = ProviderIngressRegistry::from_vars([(
            "WADDLE_PROVIDER_EXAMPLE_WEBHOOK_SECRET".to_string(),
            "shh".to_string(),
        )])
        .expect("registry build");

        let config = registry.get("example").expect("example config");
        assert_eq!(config.event_header, "x-provider-event");
        assert_eq!(config.delivery_header, "x-provider-delivery");
        assert_eq!(config.signature_header, "x-provider-signature-256");
        assert_eq!(config.signature_prefix, "sha256=");
    }

    #[test]
    fn ingress_registry_prefers_inline_secret_over_file() {
        // If both _SECRET and _SECRET_FILE are set, the inline secret wins
        // and we never read the file (so a bad path doesn't matter).
        let registry = ProviderIngressRegistry::from_vars([
            (
                "WADDLE_PROVIDER_GITHUB_WEBHOOK_SECRET".to_string(),
                "inline-wins".to_string(),
            ),
            (
                "WADDLE_PROVIDER_GITHUB_WEBHOOK_SECRET_FILE".to_string(),
                "/nonexistent/path/that/would/fail".to_string(),
            ),
        ])
        .expect("registry build");
        assert_eq!(
            registry.get("github").expect("github config").secret,
            "inline-wins"
        );
    }

    #[tokio::test]
    async fn dispatch_tracker_drains_spawned_tasks() {
        let tracker = ProviderDispatchTracker::new();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tracker.spawn(async move {
            let _ = rx.await;
        });
        tracker.close();
        let drain = tokio::spawn({
            let tracker = tracker.clone();
            async move {
                tracker.wait().await;
            }
        });
        // The drain future should still be pending until the task completes.
        tokio::task::yield_now().await;
        assert!(!drain.is_finished());
        tx.send(()).expect("notify completion");
        drain.await.expect("drain completes");
    }
}
