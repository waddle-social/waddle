use std::fmt;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::dependencies::{LocalObjectRef, SecretKeyRef};
use crate::identifiers::{ResourceName, XmppDomain};
use crate::waddle_server::conditions_schema;

/// Placement mode for a `WaddleTenant` across Waddle server cells.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TenantPlacementMode {
    /// Tenant runs on a shared multi-tenant cell pool.
    #[default]
    SharedPool,
    /// Tenant is pinned to a dedicated single-tenant cell.
    DedicatedCell,
}

/// Optional resource and spend quotas enforced for a `WaddleTenant`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TenantQuotasSpec {
    /// Maximum concurrent XMPP/WebSocket sessions for this tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_connections: Option<u32>,
    /// Maximum rooms/channels allowed in this tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rooms: Option<u32>,
    /// Maximum aggregate tenant storage in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_storage_bytes: Option<u64>,
    /// Maximum inference spend budget in microcents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_inference_spend_microcents: Option<u64>,
}

/// Extension and inference egress policy settings for a `WaddleTenant`.
#[derive(KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TenantExtensionsSpec {
    /// Extension plugin identifiers enabled for this tenant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_plugins: Vec<String>,
    /// Inference providers approved by tenant policy (e.g. `openrouter`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_providers: Vec<String>,
    /// Model identifiers approved by tenant policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_models: Vec<String>,
    /// Require Zero Data Retention (ZDR) endpoints for inference requests.
    #[serde(default)]
    pub zdr_required: bool,
}

/// Declarative specification for a `WaddleTenant` resource.
#[derive(CustomResource, KubeSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[kube(
    group = "waddle.social",
    version = "v1alpha1",
    kind = "WaddleTenant",
    plural = "waddletenants",
    namespaced,
    doc = "Declarative multi-tenant configuration and OpenFGA store lifecycle for a WaddleServer.",
    status = "WaddleTenantStatus",
    derive = "PartialEq",
    cel
)]
#[kube(shortname = "wt")]
#[kube(
    printcolumn = r#"{"name":"Tenant","type":"string","description":"Canonical tenant DNS domain","jsonPath":".spec.tenantId"}"#
)]
#[kube(
    printcolumn = r#"{"name":"Server","type":"string","description":"Referenced WaddleServer name","jsonPath":".spec.serverRef.name"}"#
)]
#[kube(
    printcolumn = r#"{"name":"Mode","type":"string","description":"Tenant placement mode","jsonPath":".spec.placementMode"}"#
)]
#[kube(
    printcolumn = r#"{"name":"Phase","type":"string","description":"Current tenant lifecycle phase","jsonPath":".status.phase"}"#
)]
#[kube(
    printcolumn = r#"{"name":"StoreID","type":"string","description":"Provisioned OpenFGA store ID","jsonPath":".status.openfgaStoreId"}"#
)]
#[kube(printcolumn(
    name = "Age",
    type_ = "date",
    json_path = ".metadata.creationTimestamp"
))]
#[serde(rename_all = "camelCase")]
pub struct WaddleTenantSpec {
    /// Reference to the `WaddleServer` hosting this tenant in the same namespace.
    pub server_ref: LocalObjectRef,

    /// Canonical lower-case DNS domain identifying the tenant (e.g. `waddle.social`).
    #[x_kube(
        validation = r#"size(self) > 0 && size(self) <= 253 && self.matches('^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$')"#
    )]
    pub tenant_id: String,

    /// Tenant placement mode (`SharedPool` or `DedicatedCell`).
    #[serde(default)]
    pub placement_mode: TenantPlacementMode,

    /// Localparts granted initial tenant owner privileges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owners: Vec<String>,

    /// Optional same-namespace Secret key holding `WADDLE_AUTH_PROVIDERS_JSON` for this tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_providers_secret_ref: Option<SecretKeyRef>,

    /// Optional per-tenant connection, room, storage, and inference spend quotas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quotas: Option<TenantQuotasSpec>,

    /// Optional per-tenant extension and inference provider allowlists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<TenantExtensionsSpec>,
}

/// Lifecycle phase reported in `WaddleTenant.status.phase`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaddleTenantPhase {
    /// Waiting for referenced `WaddleServer` to become `Ready`.
    #[default]
    Pending,
    /// Provisioning OpenFGA store/model and tenant projection ConfigMap.
    Provisioning,
    /// Tenant OpenFGA store/model and projection ConfigMap are reconciled and ready.
    Ready,
    /// Tenant spec invalid or OpenFGA/projection reconciliation encountered an error.
    Degraded,
    /// Tenant has a deletion timestamp and cleanup finalizer is running.
    Terminating,
}

impl WaddleTenantPhase {
    /// Returns the Kubernetes status phase string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Provisioning => "Provisioning",
            Self::Ready => "Ready",
            Self::Degraded => "Degraded",
            Self::Terminating => "Terminating",
        }
    }
}

/// Observed status subresource for `WaddleTenant`.
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct WaddleTenantStatus {
    /// Current tenant lifecycle phase.
    pub phase: WaddleTenantPhase,
    /// Latest `.metadata.generation` processed by the controller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Provisioned OpenFGA store ID for this tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openfga_store_id: Option<String>,
    /// Provisioned immutable OpenFGA authorization model ID for this tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openfga_model_id: Option<String>,
    /// Current placement lease epoch assigned to this tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_epoch: Option<u64>,
    /// Standard Kubernetes status conditions keyed by `type`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(schema_with = "conditions_schema")]
    pub conditions: Vec<Condition>,
}

/// Individual validation rule failure for [`WaddleTenantSpec`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TenantValidationViolation {
    /// `serverRef.name` is not a valid Kubernetes resource name.
    InvalidServerRefName,
    /// `tenantId` is not a valid canonical lower-case DNS domain.
    InvalidTenantId,
    /// `authProvidersSecretRef` contains an invalid Secret name or empty key.
    InvalidAuthProvidersSecretRef,
    /// One or more `owners` entries were empty or whitespace-only.
    EmptyOwnerEntry,
}

impl fmt::Display for TenantValidationViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidServerRefName => {
                write!(
                    f,
                    "spec.serverRef.name must be a valid lower-case Kubernetes resource name"
                )
            }
            Self::InvalidTenantId => {
                write!(
                    f,
                    "spec.tenantId must be a valid canonical lower-case DNS domain"
                )
            }
            Self::InvalidAuthProvidersSecretRef => write!(
                f,
                "spec.authProvidersSecretRef must specify a valid Secret name and non-empty key"
            ),
            Self::EmptyOwnerEntry => {
                write!(
                    f,
                    "spec.owners entries must not be empty or whitespace-only"
                )
            }
        }
    }
}

/// Aggregated validation errors for a [`WaddleTenantSpec`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid WaddleTenant specification ({} violation(s))", .violations.len())]
pub struct TenantValidationReport {
    violations: Vec<TenantValidationViolation>,
}

impl TenantValidationReport {
    /// Returns the ordered list of tenant spec validation violations.
    #[must_use]
    pub fn violations(&self) -> &[TenantValidationViolation] {
        &self.violations
    }

    /// Renders concise, semicolon-delimited diagnostic messages safe for CRD status/events.
    #[must_use]
    pub fn summary(&self) -> String {
        self.violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Validates a [`WaddleTenantSpec`] against canonical naming and reference invariants.
///
/// # Errors
/// Returns [`TenantValidationReport`] when one or more invariants fail.
pub fn validate_tenant_spec(spec: &WaddleTenantSpec) -> Result<XmppDomain, TenantValidationReport> {
    let mut violations = Vec::new();

    if ResourceName::try_new(&spec.server_ref.name).is_err() {
        violations.push(TenantValidationViolation::InvalidServerRefName);
    }

    let domain = match XmppDomain::try_new(spec.tenant_id.trim()) {
        Ok(d) => Some(d),
        Err(_) => {
            violations.push(TenantValidationViolation::InvalidTenantId);
            None
        }
    };

    if let Some(secret_ref) = &spec.auth_providers_secret_ref {
        if ResourceName::try_new(&secret_ref.name).is_err() || secret_ref.key.trim().is_empty() {
            violations.push(TenantValidationViolation::InvalidAuthProvidersSecretRef);
        }
    }

    if spec.owners.iter().any(|owner| owner.trim().is_empty()) {
        violations.push(TenantValidationViolation::EmptyOwnerEntry);
    }

    if !violations.is_empty() {
        return Err(TenantValidationReport { violations });
    }

    Ok(domain
        .unwrap_or_else(|| XmppDomain::try_new("localhost").unwrap_or_else(|_| unreachable!())))
}

#[cfg(test)]
mod tests {
    use kube::CustomResourceExt;
    use serde_json::Value;

    use super::*;

    fn sample_tenant_spec() -> WaddleTenantSpec {
        WaddleTenantSpec {
            server_ref: LocalObjectRef {
                name: "waddle".to_owned(),
            },
            tenant_id: "waddle.social".to_owned(),
            placement_mode: TenantPlacementMode::SharedPool,
            owners: vec![
                "rawkode".to_owned(),
                "icepuma".to_owned(),
                "randax".to_owned(),
            ],
            auth_providers_secret_ref: None,
            quotas: Some(TenantQuotasSpec {
                max_concurrent_connections: Some(1000),
                max_rooms: Some(500),
                max_storage_bytes: Some(10 * 1024 * 1024 * 1024),
                max_inference_spend_microcents: Some(50_000_000),
            }),
            extensions: Some(TenantExtensionsSpec {
                enabled_plugins: vec!["ai".to_owned()],
                approved_providers: vec!["openrouter".to_owned()],
                approved_models: vec!["anthropic/claude-sonnet-4".to_owned()],
                zdr_required: true,
            }),
        }
    }

    fn walk_schema_rejecting_non_structural(node: &Value, path: &str) {
        if let Some(obj) = node.as_object() {
            assert!(
                !obj.contains_key("oneOf"),
                "CRD schema at {path} must not contain non-structural oneOf"
            );
            assert!(
                !obj.contains_key("anyOf"),
                "CRD schema at {path} must not contain anyOf"
            );
            assert_ne!(
                obj.get("additionalProperties").and_then(Value::as_bool),
                Some(false),
                "CRD schema at {path} must not emit additionalProperties: false"
            );
            for (k, v) in obj {
                walk_schema_rejecting_non_structural(v, &format!("{path}.{k}"));
            }
        } else if let Some(arr) = node.as_array() {
            for (idx, item) in arr.iter().enumerate() {
                walk_schema_rejecting_non_structural(item, &format!("{path}[{idx}]"));
            }
        }
    }

    #[test]
    fn tenant_crd_schema_is_structural_and_compact() {
        let crd = WaddleTenant::crd();
        let json_val = serde_json::to_value(&crd).expect("CRD serializes to JSON");
        walk_schema_rejecting_non_structural(&json_val, "$");

        let yaml = serde_yaml_ng::to_string(&crd).expect("CRD serializes to YAML");
        assert!(
            yaml.len() < 100 * 1024,
            "WaddleTenant CRD YAML size ({} bytes) exceeds 100 KiB budget",
            yaml.len()
        );
    }

    #[test]
    fn tenant_cel_accepts_canonical_dns_and_rejects_invalid_domains() {
        let valid = WaddleTenant::new("waddle-social", sample_tenant_spec());
        assert!(
            valid.validate_cel().is_ok(),
            "valid WaddleTenant must pass CEL: {:?}",
            valid.validate_cel()
        );

        let mut upper = sample_tenant_spec();
        upper.tenant_id = "Waddle.Social".to_owned();
        assert!(
            WaddleTenant::new("bad-upper", upper)
                .validate_cel()
                .is_err(),
            "uppercase tenantId must be rejected by CEL"
        );

        let mut leading_hyphen = sample_tenant_spec();
        leading_hyphen.tenant_id = "-bad.social".to_owned();
        assert!(
            WaddleTenant::new("bad-hyphen", leading_hyphen)
                .validate_cel()
                .is_err(),
            "leading hyphen tenantId must be rejected by CEL"
        );
    }

    #[test]
    fn pure_tenant_validation_catches_invalid_refs_and_empty_owners() {
        assert!(validate_tenant_spec(&sample_tenant_spec()).is_ok());

        let mut bad = sample_tenant_spec();
        bad.server_ref.name = "INVALID_NAME".to_owned();
        bad.tenant_id = "https://waddle.social".to_owned();
        bad.owners.push("   ".to_owned());
        bad.auth_providers_secret_ref = Some(SecretKeyRef {
            name: "BAD".to_owned(),
            key: "".to_owned(),
        });

        let report = validate_tenant_spec(&bad).expect_err("invalid tenant spec must fail");
        assert!(report
            .violations()
            .contains(&TenantValidationViolation::InvalidServerRefName));
        assert!(report
            .violations()
            .contains(&TenantValidationViolation::InvalidTenantId));
        assert!(report
            .violations()
            .contains(&TenantValidationViolation::EmptyOwnerEntry));
        assert!(report
            .violations()
            .contains(&TenantValidationViolation::InvalidAuthProvidersSecretRef));
    }
}
