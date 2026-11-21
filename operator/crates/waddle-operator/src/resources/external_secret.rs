use kube::api::DynamicObject;
use serde_json::{json, Value};
use waddle_operator_crd::{EsoRemoteKeyRef, SecretStoreKind, WaddleServer};

use crate::error::OperatorError;
use crate::external_crds::DiscoveredEsoVersion;
use crate::resources::labels;

/// Builds an operator-owned ESO `ExternalSecret` targeting either `v1` or `v1beta1`.
///
/// # Errors
/// Returns [`OperatorError`] if `managed` config is missing or serialization fails.
pub fn build_managed_external_secret(
    ws: &WaddleServer,
    api_version: DiscoveredEsoVersion,
) -> Result<DynamicObject, OperatorError> {
    let name = labels::managed_external_secret_name(ws)?;
    let namespace = labels::workload_namespace(ws)?;
    let owner = labels::owner_reference(ws)?;
    let obj_labels = labels::common_labels(ws)?;
    let target_secret_name = labels::managed_target_secret_name(ws)?;

    let managed = ws
        .spec
        .secrets
        .external_secrets
        .as_ref()
        .and_then(|e| e.managed.as_ref())
        .ok_or_else(|| {
            OperatorError::InvalidClusterState("managed ExternalSecret spec missing".to_owned())
        })?;

    let mut data_entries = vec![
        remote_entry("WADDLE_SESSION_KEY", &managed.remote_refs.session_key),
        remote_entry(
            "WADDLE_OCCUPANT_ID_SECRET",
            &managed.remote_refs.occupant_id_secret,
        ),
    ];

    if let Some(ref_val) = &managed.remote_refs.spicedb_preshared_key {
        data_entries.push(remote_entry("WADDLE_SPICEDB_PRESHARED_KEY", ref_val));
    }
    if let Some(ref_val) = &managed.remote_refs.s3_access_key_id {
        data_entries.push(remote_entry("WADDLE_S3_ACCESS_KEY_ID", ref_val));
    }
    if let Some(ref_val) = &managed.remote_refs.s3_secret_access_key {
        data_entries.push(remote_entry("WADDLE_S3_SECRET_ACCESS_KEY", ref_val));
    }
    if let Some(ref_val) = &managed.remote_refs.clustering_keypair_pool {
        data_entries.push(remote_entry("WADDLE_CLUSTERING_KEYPAIR_POOL", ref_val));
    }
    if let Some(ref_val) = &managed.remote_refs.auth_providers_json {
        data_entries.push(remote_entry("WADDLE_AUTH_PROVIDERS_JSON", ref_val));
    }
    for extra in &managed.remote_refs.extra_data {
        data_entries.push(remote_entry(&extra.secret_key, &extra.remote_ref));
    }

    let store_kind = match managed.secret_store_ref.kind {
        SecretStoreKind::SecretStore => "SecretStore",
        SecretStoreKind::ClusterSecretStore => "ClusterSecretStore",
    };

    let manifest = json!({
        "apiVersion": api_version.api_version(),
        "kind": "ExternalSecret",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": obj_labels,
            "ownerReferences": [owner],
        },
        "spec": {
            "refreshInterval": managed.refresh_interval,
            "secretStoreRef": {
                "kind": store_kind,
                "name": managed.secret_store_ref.name,
            },
            "target": {
                "name": target_secret_name,
                "creationPolicy": "Owner",
            },
            "data": data_entries,
        }
    });

    serde_json::from_value(manifest).map_err(|err| OperatorError::Serialization(err.to_string()))
}

/// Compares whether the live `ExternalSecret` differs from the desired spec.
#[must_use]
pub fn managed_external_secret_drifted(live: &DynamicObject, desired: &DynamicObject) -> bool {
    let live_spec = live.data.get("spec");
    let desired_spec = desired.data.get("spec");
    let same_api = live.types.as_ref().map(|t| t.api_version.as_str())
        == desired.types.as_ref().map(|t| t.api_version.as_str());
    !(same_api && live_spec == desired_spec)
}

fn remote_entry(secret_key: &str, remote: &EsoRemoteKeyRef) -> Value {
    let mut remote_ref = serde_json::Map::new();
    remote_ref.insert("key".to_owned(), Value::String(remote.key.clone()));
    if let Some(prop) = &remote.property {
        remote_ref.insert("property".to_owned(), Value::String(prop.clone()));
    }
    if let Some(version) = &remote.version {
        remote_ref.insert("version".to_owned(), Value::String(version.clone()));
    }
    json!({
        "secretKey": secret_key,
        "remoteRef": Value::Object(remote_ref),
    })
}
