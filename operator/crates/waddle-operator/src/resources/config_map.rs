use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::ObjectMeta;
use sha2::{Digest, Sha256};
use waddle_operator_crd::{BrokerKmsProviderKind, ContentDigest, WaddleServer};

use crate::bindings::{AuthzBinding, ResolvedRuntimeBindings, StorageBinding};
use crate::error::OperatorError;
use crate::resources::labels;

/// Mount path inside `waddle-server` pods where reconciled tenant manifests live.
pub const TENANTS_MOUNT_PATH: &str = "/etc/waddle/tenants";

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

    insert_storage_env(&mut data, ws, &bindings.storage);
    insert_authz_env(&mut data, &bindings.authz);
    insert_broker_env(&mut data, ws);
    insert_extensions_env(&mut data, ws);

    data.insert(
        "WADDLE_DEFAULT_TENANT_ID".to_owned(),
        spec.xmpp.domain.clone(),
    );
    data.insert(
        "WADDLE_TENANTS_DIR".to_owned(),
        TENANTS_MOUNT_PATH.to_owned(),
    );

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

/// Builds the initial empty aggregated tenants `ConfigMap` (`<name>-tenants`) owned by `ws`.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] if `ws` lacks `name` or `uid`.
pub fn build_tenants_config_map(ws: &WaddleServer) -> Result<ConfigMap, OperatorError> {
    let cm_name = labels::tenants_config_map_name(ws)?;
    let owner = labels::owner_reference(ws)?;
    let mut obj_labels = labels::common_labels(ws)?;
    obj_labels.insert(
        labels::SERVER_LABEL.to_owned(),
        labels::workload_name(ws)?.to_owned(),
    );

    Ok(ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name),
            labels: Some(obj_labels),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        data: Some(BTreeMap::new()),
        ..ConfigMap::default()
    })
}

fn insert_storage_env(
    data: &mut BTreeMap<String, String>,
    ws: &WaddleServer,
    storage: &StorageBinding,
) {
    match storage {
        StorageBinding::Turso {
            data_dir,
            reserved_fds,
            rlimit_nofile,
        } => {
            data.insert("WADDLE_DB_DRIVER".to_owned(), "turso".to_owned());
            data.insert("WADDLE_TURSO_DATA_DIR".to_owned(), data_dir.clone());
            data.insert(
                "WADDLE_DATABASE_URL".to_owned(),
                format!(
                    "sqlite://{}/control.db?mode=rwc",
                    data_dir.trim_end_matches('/')
                ),
            );
            data.insert(
                "WADDLE_TURSO_RESERVED_FDS".to_owned(),
                reserved_fds.to_string(),
            );
            if let Some(nofile) = rlimit_nofile {
                data.insert("WADDLE_TURSO_RLIMIT_NOFILE".to_owned(), nofile.to_string());
            }
        }
        StorageBinding::Postgres(_) => {
            data.insert("WADDLE_DB_DRIVER".to_owned(), "postgres".to_owned());
            if let Some(pg) = &ws.spec.storage.postgres {
                data.insert("WADDLE_DB_POOL_SIZE".to_owned(), pg.pool_size.to_string());
                data.insert(
                    "WADDLE_DB_CONTROL_PLANE_POOL_SIZE".to_owned(),
                    pg.control_plane_pool_size.to_string(),
                );
                if let Some(timeout) = pg.acquire_timeout_secs {
                    data.insert(
                        "WADDLE_DB_ACQUIRE_TIMEOUT_SECS".to_owned(),
                        timeout.to_string(),
                    );
                }
            }
        }
    }
}

fn insert_authz_env(data: &mut BTreeMap<String, String>, authz: &AuthzBinding) {
    match authz {
        AuthzBinding::OpenFga(fga) => {
            data.insert("WADDLE_AUTHZ_BACKEND".to_owned(), "openfga".to_owned());
            data.insert("WADDLE_OPENFGA_ENDPOINT".to_owned(), fga.endpoint.clone());
            data.insert(
                "WADDLE_OPENFGA_CONSISTENCY".to_owned(),
                fga.consistency.clone(),
            );
        }
        AuthzBinding::SpiceDb(sp) => {
            data.insert("WADDLE_AUTHZ_BACKEND".to_owned(), "spicedb".to_owned());
            data.insert("WADDLE_SPICEDB_ENDPOINT".to_owned(), sp.endpoint.clone());
            data.insert(
                "WADDLE_SPICEDB_INSECURE".to_owned(),
                sp.insecure.to_string(),
            );
        }
    }
}

fn insert_broker_env(data: &mut BTreeMap<String, String>, ws: &WaddleServer) {
    let Some(broker) = &ws.spec.broker else {
        return;
    };
    data.insert(
        "WADDLE_BROKER_ENABLED".to_owned(),
        broker.enabled.to_string(),
    );
    if !broker.enabled {
        return;
    }

    data.insert("WADDLE_BROKER_DATA_DIR".to_owned(), broker.data_dir.clone());
    let admission_db = broker
        .admission_db
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(
            || format!("{}/admission.db", broker.data_dir.trim_end_matches('/')),
            str::to_owned,
        );
    data.insert("WADDLE_BROKER_ADMISSION_DB".to_owned(), admission_db);

    if let Some(spiffe) = broker
        .cell_spiffe_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        data.insert("WADDLE_CELL_SPIFFE_ID".to_owned(), spiffe.to_owned());
    }

    let Some(kms) = &broker.kms else {
        return;
    };
    data.insert(
        "WADDLE_BROKER_KMS_PROVIDER".to_owned(),
        kms.provider.as_env_value().to_owned(),
    );

    match kms.provider {
        BrokerKmsProviderKind::Scaleway => {
            if let Some(scw) = &kms.scaleway {
                data.insert("WADDLE_SCW_KMS_BASE_URL".to_owned(), scw.base_url.clone());
                data.insert("WADDLE_SCW_KMS_REGION".to_owned(), scw.region.clone());
                data.insert(
                    "WADDLE_SCW_KMS_PROJECT_ID".to_owned(),
                    scw.project_id.clone(),
                );
                if let Some(root_key_id) = scw
                    .root_key_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    data.insert(
                        "WADDLE_SCW_KMS_ROOT_KEY_ID".to_owned(),
                        root_key_id.to_owned(),
                    );
                }
            }
        }
        BrokerKmsProviderKind::Vault => {
            if let Some(vault) = &kms.vault {
                data.insert("WADDLE_VAULT_ADDR".to_owned(), vault.base_url.clone());
                data.insert("WADDLE_VAULT_TRANSIT_MOUNT".to_owned(), vault.mount.clone());
                if let Some(vault_ns) = vault
                    .namespace
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    data.insert("WADDLE_VAULT_NAMESPACE".to_owned(), vault_ns.to_owned());
                }
            }
        }
        BrokerKmsProviderKind::Local => {}
    }
}

fn insert_extensions_env(data: &mut BTreeMap<String, String>, ws: &WaddleServer) {
    let Some(ext) = &ws.spec.extensions else {
        return;
    };
    data.insert(
        "WADDLE_EXTENSIONS_ENABLED".to_owned(),
        ext.enabled.to_string(),
    );
    if let Some(dir) = ext
        .plugins_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        data.insert("WADDLE_EXTENSIONS_PLUGINS_DIR".to_owned(), dir.to_owned());
    }
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
    let mut hasher = Sha256::new();
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
