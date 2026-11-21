use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::Resource;
use waddle_operator_crd::WaddleServer;

use crate::error::OperatorError;

/// Standard label tracking ownership by this operator.
pub const MANAGED_BY_LABEL: &str = "app.kubernetes.io/managed-by";
/// Label value placed on operator-managed Secrets and resources.
pub const MANAGED_BY_VALUE: &str = "waddle-operator";

/// Returns the `WaddleServer` object name or [`OperatorError::MissingMetadata`].
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] when `.metadata.name` is missing.
pub fn workload_name(ws: &WaddleServer) -> Result<&str, OperatorError> {
    ws.metadata
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .ok_or(OperatorError::MissingMetadata {
            field: "metadata.name",
        })
}

/// Returns the `WaddleServer` namespace or [`OperatorError::MissingMetadata`].
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] when `.metadata.namespace` is missing.
pub fn workload_namespace(ws: &WaddleServer) -> Result<&str, OperatorError> {
    ws.metadata
        .namespace
        .as_deref()
        .filter(|ns| !ns.is_empty())
        .ok_or(OperatorError::MissingMetadata {
            field: "metadata.namespace",
        })
}

/// Builds the controller `OwnerReference` pointing back to `ws`.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] if `name` or `uid` is missing.
pub fn owner_reference(ws: &WaddleServer) -> Result<OwnerReference, OperatorError> {
    ws.controller_owner_ref(&())
        .ok_or(OperatorError::MissingMetadata {
            field: "metadata.uid",
        })
}

/// Standard pod/service selector labels matching the Helm chart conventions.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] when `.metadata.name` is absent.
pub fn selector_labels(ws: &WaddleServer) -> Result<BTreeMap<String, String>, OperatorError> {
    let instance = workload_name(ws)?;
    let mut map = BTreeMap::new();
    map.insert(
        "app.kubernetes.io/name".to_owned(),
        "waddle-server".to_owned(),
    );
    map.insert("app.kubernetes.io/instance".to_owned(), instance.to_owned());
    Ok(map)
}

/// Full metadata labels applied to all owned Kubernetes resources.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] when `.metadata.name` is absent.
pub fn common_labels(ws: &WaddleServer) -> Result<BTreeMap<String, String>, OperatorError> {
    let mut labels = selector_labels(ws)?;
    labels.insert(MANAGED_BY_LABEL.to_owned(), MANAGED_BY_VALUE.to_owned());
    labels.insert(
        "app.kubernetes.io/component".to_owned(),
        "server".to_owned(),
    );
    Ok(labels)
}

/// Name of the owned ConfigMap (`<name>-config`).
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] when `.metadata.name` is absent.
pub fn config_map_name(ws: &WaddleServer) -> Result<String, OperatorError> {
    Ok(format!("{}-config", workload_name(ws)?))
}

/// Name of the operator-generated YOLO Secret.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] when `.metadata.name` is absent.
pub fn yolo_secret_name(ws: &WaddleServer) -> Result<String, OperatorError> {
    let custom = ws
        .spec
        .secrets
        .yolo
        .as_ref()
        .and_then(|y| y.secret_name.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match custom {
        Some(name) => Ok(name.to_owned()),
        None => Ok(format!("{}-yolo-secrets", workload_name(ws)?)),
    }
}

/// Name of the operator-managed `ExternalSecret` and default target Secret.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] when `.metadata.name` is absent.
pub fn managed_external_secret_name(ws: &WaddleServer) -> Result<String, OperatorError> {
    Ok(format!("{}-runtime-secrets", workload_name(ws)?))
}

/// Effective target Secret name for an operator-managed `ExternalSecret`.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] when `.metadata.name` is absent.
pub fn managed_target_secret_name(ws: &WaddleServer) -> Result<String, OperatorError> {
    let custom = ws
        .spec
        .secrets
        .external_secrets
        .as_ref()
        .and_then(|e| e.managed.as_ref())
        .and_then(|m| m.target_secret_name.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match custom {
        Some(name) => Ok(name.to_owned()),
        None => managed_external_secret_name(ws),
    }
}

/// Effective PVC name (`existingClaim` or `<name>-data`).
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] when `.metadata.name` is absent.
pub fn pvc_name(ws: &WaddleServer) -> Result<String, OperatorError> {
    match ws.spec.persistence.existing_claim.as_deref() {
        Some(existing) if !existing.trim().is_empty() => Ok(existing.trim().to_owned()),
        _ => Ok(format!("{}-data", workload_name(ws)?)),
    }
}

/// Headless Service name for libp2p clustering (`<name>-headless`).
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] when `.metadata.name` is absent.
pub fn headless_service_name(ws: &WaddleServer) -> Result<String, OperatorError> {
    Ok(format!("{}-headless", workload_name(ws)?))
}

/// Optional S2S Service name (`<name>-s2s`).
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] when `.metadata.name` is absent.
pub fn s2s_service_name(ws: &WaddleServer) -> Result<String, OperatorError> {
    Ok(format!("{}-s2s", workload_name(ws)?))
}
