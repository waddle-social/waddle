//! Multi-tenant Turso + OpenFGA startup bootstrap (`WADDLE_TENANTS_DIR`,
//! `WADDLE_TURSO_*`, `WADDLE_DEFAULT_TENANT_ID`).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::permission::{PermissionBackend, TenantAuthzStoreRef};
use waddle_broker_core::quotas::TenantResourceQuotas;
use waddle_tenancy::turso_store::TursoPoolConfig;

use crate::config::TursoRuntimeConfig;
use crate::db::tenant_registry::ServerTenantRegistry;
use crate::db::turso_tenant_store::ServerTursoTenantStore;

/// Per-tenant manifest JSON written into `WADDLE_TENANTS_DIR` (`<tenant_id>.json`)
/// by the `WaddleTenant` Kubernetes operator.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantManifestSpec {
    #[serde(alias = "tenant_id")]
    pub tenant_id: String,
    #[serde(default, alias = "openfga_store_id")]
    pub openfga_store_id: Option<String>,
    #[serde(default, alias = "openfga_model_id")]
    pub openfga_model_id: Option<String>,
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default, alias = "placement_mode")]
    pub placement_mode: Option<String>,
    #[serde(default)]
    pub hostnames: Vec<String>,
}

/// Computes a deterministic SHA-256 digest over all sorted `*.json` files in
/// `dir` and returns the parsed manifests alongside that digest.
///
/// Returns `Ok(None)` when `dir` does not exist yet.
pub async fn load_tenant_manifests_with_digest(
    dir: &Path,
) -> anyhow::Result<Option<([u8; 32], Vec<TenantManifestSpec>)>> {
    if !tokio::fs::try_exists(dir).await.unwrap_or(false) {
        return Ok(None);
    }
    let mut entries = tokio::fs::read_dir(dir).await.map_err(|err| {
        anyhow::anyhow!("failed to read WADDLE_TENANTS_DIR {}: {err}", dir.display())
    })?;
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|err| {
        anyhow::anyhow!(
            "failed to iterate WADDLE_TENANTS_DIR {}: {err}",
            dir.display()
        )
    })? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut hasher = Sha256::new();
    let mut raw_files = Vec::with_capacity(paths.len());
    for path in paths {
        let raw = tokio::fs::read(&path).await.map_err(|err| {
            anyhow::anyhow!("failed to read tenant manifest {}: {err}", path.display())
        })?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        hasher.update((file_name.len() as u64).to_be_bytes());
        hasher.update(file_name.as_bytes());
        hasher.update((raw.len() as u64).to_be_bytes());
        hasher.update(&raw);
        raw_files.push((path, raw));
    }
    let digest: [u8; 32] = hasher.finalize().into();

    let mut specs = Vec::with_capacity(raw_files.len());
    for (path, raw) in raw_files {
        let spec: TenantManifestSpec = serde_json::from_slice(&raw).map_err(|err| {
            anyhow::anyhow!("failed to parse tenant manifest {}: {err}", path.display())
        })?;
        let _ = TenantId::parse(&spec.tenant_id).map_err(|err| {
            anyhow::anyhow!(
                "invalid tenant_id '{}' in {}: {err}",
                spec.tenant_id,
                path.display()
            )
        })?;
        specs.push(spec);
    }
    Ok(Some((digest, specs)))
}

/// Computes the sorted `(filename, raw_bytes)` SHA-256 digest of `*.json`
/// files in `dir` without parsing JSON, for cheap polling change detection.
pub async fn hash_tenants_dir(dir: &Path) -> anyhow::Result<Option<[u8; 32]>> {
    if !tokio::fs::try_exists(dir).await.unwrap_or(false) {
        return Ok(None);
    }
    let mut entries = tokio::fs::read_dir(dir).await.map_err(|err| {
        anyhow::anyhow!("failed to read WADDLE_TENANTS_DIR {}: {err}", dir.display())
    })?;
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|err| {
        anyhow::anyhow!(
            "failed to iterate WADDLE_TENANTS_DIR {}: {err}",
            dir.display()
        )
    })? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut hasher = Sha256::new();
    for path in paths {
        let raw = tokio::fs::read(&path).await.map_err(|err| {
            anyhow::anyhow!("failed to read tenant manifest {}: {err}", path.display())
        })?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        hasher.update((file_name.len() as u64).to_be_bytes());
        hasher.update(file_name.as_bytes());
        hasher.update((raw.len() as u64).to_be_bytes());
        hasher.update(&raw);
    }
    Ok(Some(hasher.finalize().into()))
}

/// Loads all `*.json` tenant manifest specs from `dir`.
///
/// If `dir` does not exist yet (e.g., before any `WaddleTenant` ConfigMap has
/// mounted), returns an empty list rather than failing startup.
pub async fn load_tenant_manifests(dir: &Path) -> anyhow::Result<Vec<TenantManifestSpec>> {
    if !tokio::fs::try_exists(dir).await.unwrap_or(false) {
        info!(path = %dir.display(), "WADDLE_TENANTS_DIR does not exist yet; skipping manifest scan");
        return Ok(Vec::new());
    }
    let mut entries = tokio::fs::read_dir(dir).await.map_err(|err| {
        anyhow::anyhow!("failed to read WADDLE_TENANTS_DIR {}: {err}", dir.display())
    })?;
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|err| {
        anyhow::anyhow!(
            "failed to iterate WADDLE_TENANTS_DIR {}: {err}",
            dir.display()
        )
    })? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();

    let mut specs = Vec::with_capacity(paths.len());
    for path in paths {
        let raw = tokio::fs::read_to_string(&path).await.map_err(|err| {
            anyhow::anyhow!("failed to read tenant manifest {}: {err}", path.display())
        })?;
        let spec: TenantManifestSpec = serde_json::from_str(&raw).map_err(|err| {
            anyhow::anyhow!("failed to parse tenant manifest {}: {err}", path.display())
        })?;
        let _ = TenantId::parse(&spec.tenant_id).map_err(|err| {
            anyhow::anyhow!(
                "invalid tenant_id '{}' in {}: {err}",
                spec.tenant_id,
                path.display()
            )
        })?;
        specs.push(spec);
    }
    Ok(specs)
}

/// Extracts pre-provisioned OpenFGA `(tenant_id, store_id, model_id)` entries
/// from loaded tenant manifests.
pub fn extract_openfga_store_refs(
    manifests: &[TenantManifestSpec],
) -> anyhow::Result<Vec<TenantAuthzStoreRef>> {
    let mut refs = Vec::new();
    for spec in manifests {
        let (Some(store_id), Some(model_id)) = (
            spec.openfga_store_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty()),
            spec.openfga_model_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty()),
        ) else {
            continue;
        };
        let tenant_id = TenantId::parse(&spec.tenant_id).map_err(|err| {
            anyhow::anyhow!("invalid tenant_id '{}' in manifest: {err}", spec.tenant_id)
        })?;
        refs.push(TenantAuthzStoreRef {
            tenant_id,
            store_id: store_id.to_owned(),
            model_version: model_id.to_owned(),
        });
    }
    Ok(refs)
}

/// Initializes [`ServerTursoTenantStore`] and [`ServerTenantRegistry`] under
/// `turso_cfg.data_dir` and provisions `default_tenant` plus every tenant
/// declared in `manifests`.
pub async fn bootstrap_turso_tenant_registry(
    turso_cfg: &TursoRuntimeConfig,
    default_tenant: &TenantId,
    manifests: &[TenantManifestSpec],
    permission_backend: Option<Arc<dyn PermissionBackend>>,
) -> anyhow::Result<Arc<ServerTenantRegistry>> {
    tokio::fs::create_dir_all(&turso_cfg.data_dir)
        .await
        .map_err(|err| {
            anyhow::anyhow!(
                "failed to create Turso tenant data directory {}: {err}",
                turso_cfg.data_dir.display()
            )
        })?;
    let pool_config = TursoPoolConfig {
        rlimit_nofile_override: turso_cfg.rlimit_nofile,
        reserved_fds: turso_cfg.reserved_fds,
    };
    let turso_store = ServerTursoTenantStore::new(&turso_cfg.data_dir, pool_config, None)
        .map_err(|err| anyhow::anyhow!("failed to initialize ServerTursoTenantStore: {err}"))?;
    let registry = ServerTenantRegistry::new(
        turso_store.into_store_arc(),
        permission_backend,
        TenantResourceQuotas::default(),
    )
    .map_err(|err| anyhow::anyhow!("failed to initialize ServerTenantRegistry: {err}"))?;

    let mut tenants_to_provision: Vec<TenantId> = Vec::with_capacity(1 + manifests.len());
    let mut seen = HashSet::new();
    if seen.insert(default_tenant.clone()) {
        tenants_to_provision.push(default_tenant.clone());
    }
    for spec in manifests {
        match TenantId::parse(&spec.tenant_id) {
            Ok(tid) => {
                if seen.insert(tid.clone()) {
                    tenants_to_provision.push(tid);
                }
            }
            Err(err) => {
                warn!(tenant_id = %spec.tenant_id, %err, "skipping invalid tenant manifest");
            }
        }
    }

    for tenant_id in &tenants_to_provision {
        let _ = registry.ensure_tenant(tenant_id).await.map_err(|err| {
            anyhow::anyhow!("failed to provision Turso tenant '{tenant_id}': {err}")
        })?;
        info!(tenant_id = %tenant_id, "Ensured Turso tenant store");
    }

    Ok(Arc::new(registry))
}
