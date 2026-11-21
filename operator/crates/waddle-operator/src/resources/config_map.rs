use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::ObjectMeta;
use waddle_operator_crd::{ContentDigest, WaddleServer};

use crate::bindings::ResolvedRuntimeBindings;
use crate::error::OperatorError;
use crate::resources::labels;

/// Builds the non-sensitive configuration `ConfigMap` (`<name>-config`) for `waddle-server`.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] if `ws` lacks `name` or `uid`.
pub fn build_config_map(
    ws: &WaddleServer,
    bindings: &ResolvedRuntimeBindings,
) -> Result<ConfigMap, OperatorError> {
    let cm_name = labels::config_map_name(ws)?;
    let owner = labels::owner_reference(ws)?;
    let obj_labels = labels::common_labels(ws)?;

    let mut data = BTreeMap::new();
    let spec = &ws.spec;

    data.insert(
        "WADDLE_MODE".to_owned(),
        spec.mode.as_env_value().to_owned(),
    );
    data.insert("WADDLE_BASE_URL".to_owned(), spec.base_url.clone());
    data.insert("WADDLE_DB_DRIVER".to_owned(), "postgres".to_owned());
    data.insert(
        "WADDLE_DB_POOL_SIZE".to_owned(),
        spec.postgres.pool_size.to_string(),
    );
    data.insert(
        "WADDLE_DB_CONTROL_PLANE_POOL_SIZE".to_owned(),
        spec.postgres.control_plane_pool_size.to_string(),
    );
    if let Some(timeout) = spec.postgres.acquire_timeout_secs {
        data.insert(
            "WADDLE_DB_ACQUIRE_TIMEOUT_SECS".to_owned(),
            timeout.to_string(),
        );
    }

    if let Some(uuid) = &bindings.deployment_uuid {
        data.insert("WADDLE_DEPLOYMENT_UUID".to_owned(), uuid.clone());
    }
    if let Some(action) = &bindings.lineage_action {
        data.insert("WADDLE_DB_LINEAGE_ACTION".to_owned(), action.clone());
    }

    data.insert(
        "WADDLE_XMPP_ENABLED".to_owned(),
        spec.xmpp.enabled.to_string(),
    );
    data.insert("WADDLE_XMPP_DOMAIN".to_owned(), spec.xmpp.domain.clone());
    if let Some(ws_url) = &spec.xmpp.public_websocket_url {
        if !ws_url.trim().is_empty() {
            data.insert(
                "WADDLE_XMPP_PUBLIC_WEBSOCKET_URL".to_owned(),
                ws_url.clone(),
            );
        }
    }
    data.insert(
        "WADDLE_NATIVE_AUTH_ENABLED".to_owned(),
        spec.xmpp.native_auth_enabled.to_string(),
    );
    data.insert(
        "WADDLE_REGISTRATION_ENABLED".to_owned(),
        spec.xmpp.registration_enabled.to_string(),
    );

    data.insert(
        "WADDLE_DRAIN_TIMEOUT_SECS".to_owned(),
        spec.shutdown.drain_timeout_seconds.to_string(),
    );
    data.insert(
        "WADDLE_WS_KEEPALIVE_INTERVAL_SECS".to_owned(),
        spec.ws_keepalive.interval_secs.to_string(),
    );
    data.insert(
        "WADDLE_WS_KEEPALIVE_MISS_LIMIT".to_owned(),
        spec.ws_keepalive.miss_limit.to_string(),
    );
    data.insert(
        "WADDLE_INGRESS_DB_POOL_SIZE".to_owned(),
        spec.ingress.pool_size.to_string(),
    );
    data.insert(
        "WADDLE_INGRESS_RETRY_ATTEMPTS".to_owned(),
        spec.ingress.retry_attempts.to_string(),
    );

    data.insert(
        "WADDLE_CLUSTERING_ENABLED".to_owned(),
        spec.clustering.enabled.to_string(),
    );
    data.insert(
        "WADDLE_CLUSTERING_CLAIM_RELEASE_BUDGET_MS".to_owned(),
        (spec.clustering.claim_release_budget_seconds * 1000).to_string(),
    );
    if spec.clustering.enabled {
        data.insert(
            "WADDLE_CLUSTERING_LISTEN_ADDRS".to_owned(),
            spec.clustering.listen_addrs.join(","),
        );
        let bootstrap = if spec.clustering.bootstrap_peers.is_empty() {
            format!(
                "{}:{}",
                labels::headless_service_name(ws)?,
                spec.clustering.swarm_port
            )
        } else {
            spec.clustering.bootstrap_peers.join(",")
        };
        data.insert("WADDLE_CLUSTERING_BOOTSTRAP_PEERS".to_owned(), bootstrap);
    }

    data.insert(
        "WADDLE_SERVER_OWNER_LOCALPARTS".to_owned(),
        spec.server_owner_localparts.join(","),
    );
    data.insert(
        "WADDLE_HTTP_ADDR".to_owned(),
        format!("0.0.0.0:{}", spec.service.http_port),
    );
    data.insert("RUST_LOG".to_owned(), spec.rust_log.clone());
    if let Some(cors) = &spec.cors_origins {
        if !cors.trim().is_empty() {
            data.insert("WADDLE_CORS_ORIGINS".to_owned(), cors.clone());
        }
    }

    data.insert(
        "WADDLE_SPICEDB_ENDPOINT".to_owned(),
        bindings.spicedb.endpoint.clone(),
    );
    data.insert(
        "WADDLE_SPICEDB_INSECURE".to_owned(),
        bindings.spicedb.insecure.to_string(),
    );

    if let Some(s3) = &spec.s3 {
        data.insert("WADDLE_S3_ENDPOINT".to_owned(), s3.endpoint.clone());
        data.insert("WADDLE_S3_BUCKET".to_owned(), s3.bucket.clone());
    }

    if spec.s2s.enabled {
        data.insert("WADDLE_S2S_ENABLED".to_owned(), "true".to_owned());
        data.insert(
            "WADDLE_S2S_CONFIG_FILE".to_owned(),
            "/etc/waddle/s2s/s2s.toml".to_owned(),
        );
    }

    insert_telemetry_env(&mut data, &spec.telemetry);

    for (key, value) in &spec.extra_env {
        data.insert(key.clone(), value.clone());
    }

    Ok(ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name),
            labels: Some(obj_labels),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        data: Some(data),
        ..ConfigMap::default()
    })
}

fn insert_telemetry_env(
    data: &mut BTreeMap<String, String>,
    telemetry: &waddle_operator_crd::TelemetrySpec,
) {
    if let Some(v) = telemetry.otlp_endpoint.as_deref().filter(|s| !s.is_empty()) {
        data.insert("OTEL_EXPORTER_OTLP_ENDPOINT".to_owned(), v.to_owned());
    }
    if let Some(v) = telemetry.otlp_protocol.as_deref().filter(|s| !s.is_empty()) {
        data.insert("OTEL_EXPORTER_OTLP_PROTOCOL".to_owned(), v.to_owned());
    }
    if let Some(v) = telemetry.service_name.as_deref().filter(|s| !s.is_empty()) {
        data.insert("OTEL_SERVICE_NAME".to_owned(), v.to_owned());
    }
    if let Some(v) = telemetry
        .service_version
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        data.insert("OTEL_SERVICE_VERSION".to_owned(), v.to_owned());
    }
    if let Some(v) = telemetry
        .deployment_environment
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        data.insert(
            "OTEL_RESOURCE_ATTRIBUTES".to_owned(),
            format!("deployment.environment={v}"),
        );
    }
    if let Some(v) = telemetry
        .traces_sampler
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        data.insert("OTEL_TRACES_SAMPLER".to_owned(), v.to_owned());
    }
    if let Some(v) = telemetry
        .traces_sampler_arg
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        data.insert("OTEL_TRACES_SAMPLER_ARG".to_owned(), v.to_owned());
    }
}

/// Computes a deterministic SHA-256 hex digest of a `ConfigMap`'s entries.
///
/// # Errors
/// Returns [`OperatorError::Serialization`] only if digest construction fails unexpectedly.
pub fn config_map_digest(cm: &ConfigMap) -> Result<ContentDigest, OperatorError> {
    use sha2::Digest;

    let mut hasher = sha2::Sha256::new();
    if let Some(data) = &cm.data {
        for (key, value) in data {
            hasher.update((key.len() as u64).to_be_bytes());
            hasher.update(key.as_bytes());
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
    }
    let hex_digest = hex::encode(hasher.finalize());
    ContentDigest::try_new(&hex_digest).map_err(|err| OperatorError::Serialization(err.to_string()))
}
