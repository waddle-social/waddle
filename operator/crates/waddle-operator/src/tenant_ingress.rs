//! Per-tenant Gateway API, cert-manager, and ExternalDNS resource builders and reconciler.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, Patch, PatchParams, Preconditions};
use kube::{Client, ResourceExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use waddle_operator_crd::{
    IngressSpec, ResolvedIngressMode, ResolvedTenantIngress, TenantIngressMode, WaddleServer,
    WaddleTenant,
};

use crate::error::OperatorError;
use crate::external_crds::api_resource_certificate;
use crate::leader_election::LeadershipFence;
use crate::resources;

/// Maximum listeners permitted on a single Kubernetes Gateway API `Gateway`.
pub const MAX_GATEWAY_LISTENERS: usize = 64;

const FIELD_MANAGER_TENANT: &str = "waddle-operator-tenant";
const DNS_RECORD_TTL: u32 = 300;

/// Dynamic [`ApiResource`] descriptor for Gateway API `HTTPRoute` (`gateway.networking.k8s.io/v1`).
#[must_use]
pub fn api_resource_http_route() -> ApiResource {
    ApiResource {
        group: "gateway.networking.k8s.io".to_owned(),
        version: "v1".to_owned(),
        api_version: "gateway.networking.k8s.io/v1".to_owned(),
        kind: "HTTPRoute".to_owned(),
        plural: "httproutes".to_owned(),
    }
}

/// Dynamic [`ApiResource`] descriptor for Gateway API `Gateway` (`gateway.networking.k8s.io/v1`).
#[must_use]
pub fn api_resource_gateway() -> ApiResource {
    ApiResource {
        group: "gateway.networking.k8s.io".to_owned(),
        version: "v1".to_owned(),
        api_version: "gateway.networking.k8s.io/v1".to_owned(),
        kind: "Gateway".to_owned(),
        plural: "gateways".to_owned(),
    }
}

/// Dynamic [`ApiResource`] descriptor for Gateway API `ReferenceGrant` (`gateway.networking.k8s.io/v1beta1`).
#[must_use]
pub fn api_resource_reference_grant() -> ApiResource {
    ApiResource {
        group: "gateway.networking.k8s.io".to_owned(),
        version: "v1beta1".to_owned(),
        api_version: "gateway.networking.k8s.io/v1beta1".to_owned(),
        kind: "ReferenceGrant".to_owned(),
        plural: "referencegrants".to_owned(),
    }
}

/// Dynamic [`ApiResource`] descriptor for ExternalDNS `DNSEndpoint` (`externaldns.k8s.io/v1alpha1`).
#[must_use]
pub fn api_resource_dns_endpoint() -> ApiResource {
    ApiResource {
        group: "externaldns.k8s.io".to_owned(),
        version: "v1alpha1".to_owned(),
        api_version: "externaldns.k8s.io/v1alpha1".to_owned(),
        kind: "DNSEndpoint".to_owned(),
        plural: "dnsendpoints".to_owned(),
    }
}

/// Name of the tenant HTTPS `HTTPRoute` (`waddle-tenant-<cr>-https`).
#[must_use]
pub fn tenant_https_route_name(cr_name: &str) -> String {
    format!("waddle-tenant-{cr_name}-https")
}

/// Name of the tenant HTTP->HTTPS redirect `HTTPRoute` (`waddle-tenant-<cr>-http-redirect`).
#[must_use]
pub fn tenant_http_redirect_route_name(cr_name: &str) -> String {
    format!("waddle-tenant-{cr_name}-http-redirect")
}

/// Name of the managed-subdomain `DNSEndpoint` (`waddle-tenant-<cr>`).
#[must_use]
pub fn tenant_dns_endpoint_name(cr_name: &str) -> String {
    format!("waddle-tenant-{cr_name}")
}

/// Name of the BYO `cert-manager` `Certificate` and target TLS `Secret` (`waddle-tenant-<cr>-tls`).
#[must_use]
pub fn tenant_certificate_name(cr_name: &str) -> String {
    format!("waddle-tenant-{cr_name}-tls")
}

/// Name of the per-tenant exact-Secret `ReferenceGrant` (`waddle-tenant-<cr>-tls-grant`).
#[must_use]
pub fn tenant_reference_grant_name(cr_name: &str) -> String {
    format!("waddle-tenant-{cr_name}-tls-grant")
}

/// Deterministic Gateway API listener name for a BYO hostname (`byo-<sha256_12(hostname)>`).
#[must_use]
pub fn byo_listener_name(hostname: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(hostname.to_ascii_lowercase().as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("byo-{}", &digest[..12])
}

/// Per-tenant Server-Side Apply `fieldManager` used when patching `Gateway.spec.listeners`.
#[must_use]
pub fn tenant_gateway_field_manager(namespace: &str, cr_name: &str) -> String {
    format!("waddle-operator-tenant.{namespace}.{cr_name}")
}

/// Desired BYO HTTPS listener entry before capacity/collision checks and SSA encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByoListenerSpec {
    /// Listener `name` (`byo-<sha256_12(host)>`).
    pub name: String,
    /// Hostname matched by SNI and `Host`.
    pub hostname: String,
    /// Secret name holding the tenant TLS certificate in `secret_namespace`.
    pub secret_name: String,
    /// Namespace containing `secret_name`.
    pub secret_namespace: String,
}

impl ByoListenerSpec {
    /// Serializes this listener to the Gateway API v1 `spec.listeners[]` JSON shape.
    #[must_use]
    pub fn to_listener_json(&self) -> Value {
        json!({
            "name": self.name,
            "protocol": "HTTPS",
            "port": 443,
            "hostname": self.hostname,
            "tls": {
                "mode": "Terminate",
                "certificateRefs": [{
                    "group": "",
                    "kind": "Secret",
                    "name": self.secret_name,
                    "namespace": self.secret_namespace,
                }],
            },
            "allowedRoutes": {
                "namespaces": {
                    "from": "All",
                },
            },
        })
    }
}

/// Rejection returned when a BYO `Gateway` listener cannot be safely attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayListenerRejection {
    /// Adding the listener(s) would exceed the Kubernetes Gateway CRD `maxItems: 64` cap.
    CapacityExceeded {
        /// Number of existing listeners not managed by this tenant.
        existing_other_listeners: usize,
        /// Number of listeners this tenant needs to add.
        requested: usize,
    },
    /// Desired hostname or listener name collides with a listener owned by another manager.
    HostnameCollision {
        /// Conflicting hostname.
        hostname: String,
        /// Existing listener name on the `Gateway`.
        existing_listener: String,
        /// Owning `fieldManager` when recorded in `metadata.managedFields`.
        owner: Option<String>,
    },
}

/// Outcome of reconciling per-tenant ingress resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressReconcileOutcome {
    /// Ingress management is disabled for this tenant or server.
    Disabled,
    /// All ingress resources (`HTTPRoute`s, `DNSEndpoint` or `Certificate`+`ReferenceGrant`+`Gateway`) are ready.
    Ready {
        /// Machine-readable reason for `TlsReady`.
        tls_reason: &'static str,
        /// Diagnostic message for `TlsReady`.
        tls_message: String,
        /// Diagnostic message for `IngressReady`.
        ingress_message: String,
    },
    /// BYO `Certificate` is still issuing or its TLS `Secret` is not yet populated.
    WaitingForCertificate {
        /// Human-readable status explanation.
        message: String,
    },
    /// Ingress configuration, Gateway capacity, or listener collision prevented reconciliation.
    Degraded {
        /// Condition reason (`IngressConfigIncomplete`, `GatewayNotFound`, `GatewayCapacityExceeded`, `GatewayHostnameConflict`).
        reason: &'static str,
        /// Human-readable diagnostic message.
        message: String,
    },
}

/// Builds the SSA `Gateway` patch payload containing only the tenant's listener slice
/// (or an empty `listeners: []` slice on deletion to release only this tenant's ownership).
#[must_use]
pub fn build_gateway_ssa_patch(
    gateway_name: &str,
    gateway_namespace: &str,
    listeners: &[Value],
) -> Value {
    json!({
        "apiVersion": "gateway.networking.k8s.io/v1",
        "kind": "Gateway",
        "metadata": {
            "name": gateway_name,
            "namespace": gateway_namespace,
        },
        "spec": {
            "listeners": listeners,
        },
    })
}

/// Checks `Gateway.spec.listeners` and `metadata.managedFields` to prevent:
/// 1. Exceeding the 64-listener Gateway API CRD cap, and
/// 2. Colliding with any listener owned by a different `fieldManager` (`kustomize-controller` or another tenant).
///
/// # Errors
/// Returns [`GatewayListenerRejection`] when attaching `desired` would collide or overflow capacity.
pub fn check_gateway_listener_conflicts_and_capacity(
    gateway: &DynamicObject,
    desired: &[ByoListenerSpec],
    field_manager: &str,
) -> Result<(), GatewayListenerRejection> {
    let owners = listener_owners_from_managed_fields(gateway);
    let existing = gateway
        .data
        .get("spec")
        .and_then(|s| s.get("listeners"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let desired_names: BTreeSet<&str> = desired.iter().map(|d| d.name.as_str()).collect();

    for listener in existing {
        let Some(existing_name) = listener.get("name").and_then(Value::as_str) else {
            continue;
        };
        let existing_host = listener.get("hostname").and_then(Value::as_str);
        let existing_port = listener.get("port").and_then(Value::as_i64);
        let other_owner = owners
            .get(existing_name)
            .and_then(|mgrs| mgrs.iter().find(|m| m.as_str() != field_manager))
            .cloned();

        for wanted in desired {
            if existing_name == wanted.name && other_owner.is_some() {
                return Err(GatewayListenerRejection::HostnameCollision {
                    hostname: wanted.hostname.clone(),
                    existing_listener: existing_name.to_owned(),
                    owner: other_owner,
                });
            }
            if existing_port == Some(443)
                && existing_host.is_some_and(|h| h.eq_ignore_ascii_case(&wanted.hostname))
                && (existing_name != wanted.name || other_owner.is_some())
            {
                return Err(GatewayListenerRejection::HostnameCollision {
                    hostname: wanted.hostname.clone(),
                    existing_listener: existing_name.to_owned(),
                    owner: other_owner,
                });
            }
        }
    }

    let existing_other_listeners = existing
        .iter()
        .filter(|listener| {
            listener
                .get("name")
                .and_then(Value::as_str)
                .is_none_or(|name| !desired_names.contains(name))
        })
        .count();

    if existing_other_listeners.saturating_add(desired.len()) > MAX_GATEWAY_LISTENERS {
        return Err(GatewayListenerRejection::CapacityExceeded {
            existing_other_listeners,
            requested: desired.len(),
        });
    }

    Ok(())
}

/// Builds a per-tenant exact-Secret `ReferenceGrant` permitting `Gateway` in `gateway_namespace`
/// to reference ONLY `secret_name` in the tenant's namespace.
#[must_use]
pub fn build_reference_grant_manifest(
    grant_name: &str,
    namespace: &str,
    gateway_namespace: &str,
    secret_name: &str,
    labels: &BTreeMap<String, String>,
    owner: &OwnerReference,
) -> Value {
    json!({
        "apiVersion": "gateway.networking.k8s.io/v1beta1",
        "kind": "ReferenceGrant",
        "metadata": {
            "name": grant_name,
            "namespace": namespace,
            "labels": labels,
            "ownerReferences": [owner],
        },
        "spec": {
            "from": [{
                "group": "gateway.networking.k8s.io",
                "kind": "Gateway",
                "namespace": gateway_namespace,
            }],
            "to": [{
                "group": "",
                "kind": "Secret",
                "name": secret_name,
            }],
        },
    })
}

/// Builds a `cert-manager.io/v1` `Certificate` manifest for a BYO `CustomDomain` tenant.
#[must_use]
pub fn build_certificate_manifest(
    cert_name: &str,
    namespace: &str,
    cluster_issuer: &str,
    dns_names: &[String],
    labels: &BTreeMap<String, String>,
    owner: &OwnerReference,
) -> Value {
    json!({
        "apiVersion": "cert-manager.io/v1",
        "kind": "Certificate",
        "metadata": {
            "name": cert_name,
            "namespace": namespace,
            "labels": labels,
            "ownerReferences": [owner],
        },
        "spec": {
            "secretName": cert_name,
            "issuerRef": {
                "name": cluster_issuer,
                "kind": "ClusterIssuer",
                "group": "cert-manager.io",
            },
            "dnsNames": dns_names,
        },
    })
}

/// Target Gateway API `Gateway` + backend `Service` parameters for building `HTTPRoute` manifests.
#[derive(Debug, Clone, Copy)]
pub struct HttpRouteTarget<'a> {
    /// Target `Gateway` name.
    pub gateway_name: &'a str,
    /// Target `Gateway` namespace.
    pub gateway_namespace: &'a str,
    /// Backend `Service` name in the tenant's namespace.
    pub backend_service: &'a str,
    /// Backend `Service` HTTP port.
    pub backend_port: i32,
}

/// Builds a Gateway API `HTTPRoute` forwarding HTTPS traffic to the hosting `WaddleServer` Service.
#[must_use]
pub fn build_https_route_manifest(
    route_name: &str,
    namespace: &str,
    target: HttpRouteTarget<'_>,
    section_names: &[String],
    hostnames: &[String],
    labels: &BTreeMap<String, String>,
    owner: &OwnerReference,
) -> Value {
    let parent_refs: Vec<Value> = section_names
        .iter()
        .map(|section| {
            json!({
                "name": target.gateway_name,
                "namespace": target.gateway_namespace,
                "sectionName": section,
            })
        })
        .collect();
    json!({
        "apiVersion": "gateway.networking.k8s.io/v1",
        "kind": "HTTPRoute",
        "metadata": {
            "name": route_name,
            "namespace": namespace,
            "labels": labels,
            "ownerReferences": [owner],
        },
        "spec": {
            "parentRefs": parent_refs,
            "hostnames": hostnames,
            "rules": [{
                "backendRefs": [{
                    "name": target.backend_service,
                    "port": target.backend_port,
                }],
            }],
        },
    })
}

/// Builds a Gateway API `HTTPRoute` performing a 301 HTTP->HTTPS redirect on `http_section`.
#[must_use]
pub fn build_http_redirect_route_manifest(
    route_name: &str,
    namespace: &str,
    target: HttpRouteTarget<'_>,
    http_section: &str,
    hostnames: &[String],
    labels: &BTreeMap<String, String>,
    owner: &OwnerReference,
) -> Value {
    json!({
        "apiVersion": "gateway.networking.k8s.io/v1",
        "kind": "HTTPRoute",
        "metadata": {
            "name": route_name,
            "namespace": namespace,
            "labels": labels,
            "ownerReferences": [owner],
        },
        "spec": {
            "parentRefs": [{
                "name": target.gateway_name,
                "namespace": target.gateway_namespace,
                "sectionName": http_section,
            }],
            "hostnames": hostnames,
            "rules": [{
                "filters": [{
                    "type": "RequestRedirect",
                    "requestRedirect": {
                        "scheme": "https",
                        "statusCode": 301,
                    },
                }],
            }],
        },
    })
}

/// Builds an ExternalDNS `DNSEndpoint` for `ManagedSubdomain` hostnames pointing to `target_ips`.
#[must_use]
pub fn build_dns_endpoint_manifest(
    name: &str,
    namespace: &str,
    hostnames: &[String],
    target_ips: &[String],
    labels: &BTreeMap<String, String>,
    owner: &OwnerReference,
) -> Value {
    let mut ipv4 = Vec::new();
    let mut ipv6 = Vec::new();
    for raw in target_ips {
        match raw.parse::<IpAddr>() {
            Ok(IpAddr::V4(_)) => ipv4.push(raw.clone()),
            Ok(IpAddr::V6(_)) => ipv6.push(raw.clone()),
            Err(_) => {}
        }
    }

    let mut endpoints = Vec::new();
    for host in hostnames {
        if !ipv4.is_empty() {
            endpoints.push(json!({
                "dnsName": host,
                "recordTTL": DNS_RECORD_TTL,
                "recordType": "A",
                "targets": ipv4,
            }));
        }
        if !ipv6.is_empty() {
            endpoints.push(json!({
                "dnsName": host,
                "recordTTL": DNS_RECORD_TTL,
                "recordType": "AAAA",
                "targets": ipv6,
            }));
        }
    }

    json!({
        "apiVersion": "externaldns.k8s.io/v1alpha1",
        "kind": "DNSEndpoint",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": labels,
            "ownerReferences": [owner],
        },
        "spec": {
            "endpoints": endpoints,
        },
    })
}

struct IngressReconcileParams<'a> {
    client: &'a Client,
    ns: &'a str,
    cr_name: &'a str,
    target: HttpRouteTarget<'a>,
    server_ingress: &'a IngressSpec,
    hostnames: &'a [String],
    labels: &'a BTreeMap<String, String>,
    owner: &'a OwnerReference,
    leadership: &'a LeadershipFence,
}

/// Reconciles per-tenant ingress resources according to `resolved` and `ws.spec.ingress`.
///
/// # Errors
/// Returns [`OperatorError`] on leadership loss or unexpected Kubernetes API errors.
pub async fn reconcile_tenant_ingress(
    client: &Client,
    ns: &str,
    ws: &WaddleServer,
    wt: &WaddleTenant,
    resolved: &ResolvedTenantIngress,
    leadership: &LeadershipFence,
) -> Result<IngressReconcileOutcome, OperatorError> {
    if resolved.mode == ResolvedIngressMode::Disabled {
        return Ok(IngressReconcileOutcome::Disabled);
    }

    let cr_name = resources::tenant_name(wt)?;
    let server_name = resources::workload_name(ws)?;
    let owner = resources::tenant_owner_reference(wt)?;
    let labels = tenant_ingress_labels(server_name, &wt.spec.tenant_id);
    let server_ingress = &ws.spec.ingress;

    let Some(gw_ref) = &server_ingress.gateway_ref else {
        return Ok(IngressReconcileOutcome::Degraded {
            reason: "IngressConfigIncomplete",
            message: "WaddleServer spec.ingress.gatewayRef is required when ingress is enabled"
                .to_owned(),
        });
    };

    let params = IngressReconcileParams {
        client,
        ns,
        cr_name,
        target: HttpRouteTarget {
            gateway_name: &gw_ref.name,
            gateway_namespace: &gw_ref.namespace,
            backend_service: server_name,
            backend_port: ws.spec.service.http_port,
        },
        server_ingress,
        hostnames: &resolved.hostnames,
        labels: &labels,
        owner: &owner,
        leadership,
    };

    match resolved.mode {
        ResolvedIngressMode::Disabled => Ok(IngressReconcileOutcome::Disabled),
        ResolvedIngressMode::ManagedSubdomain => reconcile_managed_subdomain(&params).await,
        ResolvedIngressMode::CustomDomain => reconcile_custom_domain(&params).await,
    }
}

async fn reconcile_managed_subdomain(
    p: &IngressReconcileParams<'_>,
) -> Result<IngressReconcileOutcome, OperatorError> {
    let Some(https_section) = p
        .server_ingress
        .managed_wildcard_https_section
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(IngressReconcileOutcome::Degraded {
            reason: "IngressConfigIncomplete",
            message: "WaddleServer spec.ingress.managedWildcardHttpsSection is required for ManagedSubdomain".to_owned(),
        });
    };
    let Some(http_section) = p
        .server_ingress
        .managed_wildcard_http_section
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(IngressReconcileOutcome::Degraded {
            reason: "IngressConfigIncomplete",
            message: "WaddleServer spec.ingress.managedWildcardHttpSection is required for ManagedSubdomain".to_owned(),
        });
    };

    let https_name = tenant_https_route_name(p.cr_name);
    let https_route = build_https_route_manifest(
        &https_name,
        p.ns,
        p.target,
        &[https_section.to_owned()],
        p.hostnames,
        p.labels,
        p.owner,
    );
    apply_dynamic(
        p.client,
        p.leadership,
        DynamicApplySpec {
            namespace: p.ns,
            api_resource: &api_resource_http_route(),
            name: &https_name,
            manifest: &https_route,
            field_manager: FIELD_MANAGER_TENANT,
            context: "apply managed tenant HTTPS HTTPRoute",
        },
    )
    .await?;

    let redirect_name = tenant_http_redirect_route_name(p.cr_name);
    let redirect_route = build_http_redirect_route_manifest(
        &redirect_name,
        p.ns,
        p.target,
        http_section,
        p.hostnames,
        p.labels,
        p.owner,
    );
    apply_dynamic(
        p.client,
        p.leadership,
        DynamicApplySpec {
            namespace: p.ns,
            api_resource: &api_resource_http_route(),
            name: &redirect_name,
            manifest: &redirect_route,
            field_manager: FIELD_MANAGER_TENANT,
            context: "apply managed tenant HTTP redirect HTTPRoute",
        },
    )
    .await?;

    if !p.server_ingress.target_ips.is_empty() {
        let dns_name = tenant_dns_endpoint_name(p.cr_name);
        let dns_endpoint = build_dns_endpoint_manifest(
            &dns_name,
            p.ns,
            p.hostnames,
            &p.server_ingress.target_ips,
            p.labels,
            p.owner,
        );
        apply_dynamic(
            p.client,
            p.leadership,
            DynamicApplySpec {
                namespace: p.ns,
                api_resource: &api_resource_dns_endpoint(),
                name: &dns_name,
                manifest: &dns_endpoint,
                field_manager: FIELD_MANAGER_TENANT,
                context: "apply managed tenant DNSEndpoint",
            },
        )
        .await?;
    }

    Ok(IngressReconcileOutcome::Ready {
        tls_reason: "WildcardCertificate",
        tls_message: format!("Using managed wildcard TLS listener {https_section}"),
        ingress_message: format!(
            "Reconciled managed HTTPRoutes and DNSEndpoint for {}",
            p.hostnames.join(", ")
        ),
    })
}

async fn reconcile_custom_domain(
    p: &IngressReconcileParams<'_>,
) -> Result<IngressReconcileOutcome, OperatorError> {
    let Some(cluster_issuer) = p
        .server_ingress
        .tls_cluster_issuer
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(IngressReconcileOutcome::Degraded {
            reason: "IngressConfigIncomplete",
            message: "WaddleServer spec.ingress.tlsClusterIssuer is required for CustomDomain"
                .to_owned(),
        });
    };
    let Some(byo_http_section) = p
        .server_ingress
        .byo_http_section
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(IngressReconcileOutcome::Degraded {
            reason: "IngressConfigIncomplete",
            message: "WaddleServer spec.ingress.byoHttpSection is required for CustomDomain"
                .to_owned(),
        });
    };

    let cert_name = tenant_certificate_name(p.cr_name);
    let cert_manifest = build_certificate_manifest(
        &cert_name,
        p.ns,
        cluster_issuer,
        p.hostnames,
        p.labels,
        p.owner,
    );
    apply_dynamic(
        p.client,
        p.leadership,
        DynamicApplySpec {
            namespace: p.ns,
            api_resource: &api_resource_certificate(),
            name: &cert_name,
            manifest: &cert_manifest,
            field_manager: FIELD_MANAGER_TENANT,
            context: "apply tenant Certificate",
        },
    )
    .await?;

    let grant_name = tenant_reference_grant_name(p.cr_name);
    let grant_manifest = build_reference_grant_manifest(
        &grant_name,
        p.ns,
        p.target.gateway_namespace,
        &cert_name,
        p.labels,
        p.owner,
    );
    apply_dynamic(
        p.client,
        p.leadership,
        DynamicApplySpec {
            namespace: p.ns,
            api_resource: &api_resource_reference_grant(),
            name: &grant_name,
            manifest: &grant_manifest,
            field_manager: FIELD_MANAGER_TENANT,
            context: "apply per-tenant exact-Secret ReferenceGrant",
        },
    )
    .await?;

    let cert_api: Api<DynamicObject> =
        Api::namespaced_with(p.client.clone(), p.ns, &api_resource_certificate());
    let cert_obj = cert_api
        .get_opt(&cert_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get tenant Certificate status",
            source,
        })?;
    let cert_ready = cert_obj.as_ref().is_some_and(is_certificate_ready);
    if !cert_ready {
        return Ok(IngressReconcileOutcome::WaitingForCertificate {
            message: format!(
                "Waiting for Certificate {cert_name} to reach Ready=True before attaching Gateway listener"
            ),
        });
    }

    let secret_ready = is_tls_secret_populated(p.client, p.ns, &cert_name).await?;
    if !secret_ready {
        return Ok(IngressReconcileOutcome::WaitingForCertificate {
            message: format!(
                "Certificate {cert_name} is Ready but TLS Secret {cert_name} is not populated yet"
            ),
        });
    }

    let gw_api: Api<DynamicObject> = Api::namespaced_with(
        p.client.clone(),
        p.target.gateway_namespace,
        &api_resource_gateway(),
    );
    let Some(gateway_obj) = gw_api
        .get_opt(p.target.gateway_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get target Gateway for BYO listener attachment",
            source,
        })?
    else {
        return Ok(IngressReconcileOutcome::Degraded {
            reason: "GatewayNotFound",
            message: format!(
                "Target Gateway {}/{} not found",
                p.target.gateway_namespace, p.target.gateway_name
            ),
        });
    };

    let desired_listeners: Vec<ByoListenerSpec> = p
        .hostnames
        .iter()
        .map(|host| ByoListenerSpec {
            name: byo_listener_name(host),
            hostname: host.clone(),
            secret_name: cert_name.clone(),
            secret_namespace: p.ns.to_owned(),
        })
        .collect();
    let field_manager = tenant_gateway_field_manager(p.ns, p.cr_name);

    if let Err(rejection) = check_gateway_listener_conflicts_and_capacity(
        &gateway_obj,
        &desired_listeners,
        &field_manager,
    ) {
        return Ok(match rejection {
            GatewayListenerRejection::CapacityExceeded {
                existing_other_listeners,
                requested,
            } => IngressReconcileOutcome::Degraded {
                reason: "GatewayCapacityExceeded",
                message: format!(
                    "Gateway {}/{} has {existing_other_listeners} existing listeners; adding {requested} exceeds cap {MAX_GATEWAY_LISTENERS}",
                    p.target.gateway_namespace, p.target.gateway_name
                ),
            },
            GatewayListenerRejection::HostnameCollision {
                hostname,
                existing_listener,
                owner,
            } => {
                let owner_desc = owner.unwrap_or_else(|| "another manager".to_owned());
                IngressReconcileOutcome::Degraded {
                    reason: "GatewayHostnameConflict",
                    message: format!(
                        "Hostname {hostname} collides with existing listener {existing_listener} owned by {owner_desc} on Gateway {}/{}",
                        p.target.gateway_namespace, p.target.gateway_name
                    ),
                }
            }
        });
    }

    let listener_values: Vec<Value> = desired_listeners
        .iter()
        .map(ByoListenerSpec::to_listener_json)
        .collect();
    let gw_patch = build_gateway_ssa_patch(
        p.target.gateway_name,
        p.target.gateway_namespace,
        &listener_values,
    );
    apply_dynamic(
        p.client,
        p.leadership,
        DynamicApplySpec {
            namespace: p.target.gateway_namespace,
            api_resource: &api_resource_gateway(),
            name: p.target.gateway_name,
            manifest: &gw_patch,
            field_manager: &field_manager,
            context: "apply BYO tenant listener to Gateway",
        },
    )
    .await?;

    let section_names: Vec<String> = desired_listeners.iter().map(|l| l.name.clone()).collect();
    let https_name = tenant_https_route_name(p.cr_name);
    let https_route = build_https_route_manifest(
        &https_name,
        p.ns,
        p.target,
        &section_names,
        p.hostnames,
        p.labels,
        p.owner,
    );
    apply_dynamic(
        p.client,
        p.leadership,
        DynamicApplySpec {
            namespace: p.ns,
            api_resource: &api_resource_http_route(),
            name: &https_name,
            manifest: &https_route,
            field_manager: FIELD_MANAGER_TENANT,
            context: "apply BYO tenant HTTPS HTTPRoute",
        },
    )
    .await?;

    let redirect_name = tenant_http_redirect_route_name(p.cr_name);
    let redirect_route = build_http_redirect_route_manifest(
        &redirect_name,
        p.ns,
        p.target,
        byo_http_section,
        p.hostnames,
        p.labels,
        p.owner,
    );
    apply_dynamic(
        p.client,
        p.leadership,
        DynamicApplySpec {
            namespace: p.ns,
            api_resource: &api_resource_http_route(),
            name: &redirect_name,
            manifest: &redirect_route,
            field_manager: FIELD_MANAGER_TENANT,
            context: "apply BYO tenant HTTP redirect HTTPRoute",
        },
    )
    .await?;

    Ok(IngressReconcileOutcome::Ready {
        tls_reason: "CertificateReady",
        tls_message: format!("Certificate {cert_name} is Ready and TLS Secret is populated"),
        ingress_message: format!(
            "Reconciled BYO Gateway listener, ReferenceGrant, and HTTPRoutes for {}",
            p.hostnames.join(", ")
        ),
    })
}

/// Removes any BYO `Gateway` listener owned by this tenant's SSA `fieldManager` and deletes
/// tenant-owned ingress resources (`HTTPRoute`s, `DNSEndpoint`, `Certificate`, `ReferenceGrant`).
///
/// # Errors
/// Returns [`OperatorError`] on leadership loss or unexpected Kubernetes API failures.
pub async fn cleanup_tenant_ingress(
    client: &Client,
    ns: &str,
    cr_name: &str,
    wt: &WaddleTenant,
    ws: Option<&WaddleServer>,
    leadership: &LeadershipFence,
) -> Result<(), OperatorError> {
    let ingress_enabled = ws.is_some_and(|s| s.spec.ingress.enabled);
    let mode = wt.spec.effective_ingress_mode();

    if ingress_enabled {
        if let Some(gw_ref) = ws.and_then(|s| s.spec.ingress.gateway_ref.as_ref()) {
            let field_manager = tenant_gateway_field_manager(ns, cr_name);
            let gw_api: Api<DynamicObject> =
                Api::namespaced_with(client.clone(), &gw_ref.namespace, &api_resource_gateway());
            match gw_api.get_opt(&gw_ref.name).await {
                Ok(Some(gw_obj)) => {
                    let hostnames = wt.spec.effective_hostnames();
                    let owns_listener = gateway_has_field_manager(&gw_obj, &field_manager)
                        || mode == TenantIngressMode::CustomDomain
                        || gateway_has_byo_listener_for_hosts(&gw_obj, &hostnames);
                    if owns_listener {
                        let empty_patch =
                            build_gateway_ssa_patch(&gw_ref.name, &gw_ref.namespace, &[]);
                        apply_dynamic(
                            client,
                            leadership,
                            DynamicApplySpec {
                                namespace: &gw_ref.namespace,
                                api_resource: &api_resource_gateway(),
                                name: &gw_ref.name,
                                manifest: &empty_patch,
                                field_manager: &field_manager,
                                context: "remove BYO tenant listener from Gateway",
                            },
                        )
                        .await?;
                    }
                }
                Ok(None) => {}
                Err(kube::Error::Api(status)) if status.code == 404 => {}
                Err(source) => {
                    return Err(OperatorError::Kube {
                        context: "get Gateway during tenant cleanup",
                        source,
                    });
                }
            }
        }
    }

    if ingress_enabled || wt.spec.ingress.is_some() {
        let owner_uid = wt.metadata.uid.as_deref();
        let targets = [
            (
                api_resource_http_route(),
                tenant_https_route_name(cr_name),
                "delete tenant HTTPS HTTPRoute",
            ),
            (
                api_resource_http_route(),
                tenant_http_redirect_route_name(cr_name),
                "delete tenant HTTP redirect HTTPRoute",
            ),
            (
                api_resource_dns_endpoint(),
                tenant_dns_endpoint_name(cr_name),
                "delete tenant DNSEndpoint",
            ),
            (
                api_resource_certificate(),
                tenant_certificate_name(cr_name),
                "delete tenant Certificate",
            ),
            (
                api_resource_reference_grant(),
                tenant_reference_grant_name(cr_name),
                "delete tenant ReferenceGrant",
            ),
        ];

        for (ar, name, context) in targets {
            delete_dynamic_if_owned(client, ns, &ar, &name, owner_uid, leadership, context).await?;
        }
    }

    Ok(())
}

fn tenant_ingress_labels(server_name: &str, tenant_id: &str) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert(
        resources::MANAGED_BY_LABEL.to_owned(),
        resources::MANAGED_BY_VALUE.to_owned(),
    );
    labels.insert(
        "app.kubernetes.io/component".to_owned(),
        "tenant-ingress".to_owned(),
    );
    labels.insert(resources::SERVER_LABEL.to_owned(), server_name.to_owned());
    labels.insert(resources::TENANT_LABEL.to_owned(), tenant_id.to_owned());
    labels
}

fn is_certificate_ready(cert: &DynamicObject) -> bool {
    cert.data
        .get("status")
        .and_then(|s| s.get("conditions"))
        .and_then(Value::as_array)
        .is_some_and(|conds| {
            conds.iter().any(|c| {
                c.get("type").and_then(Value::as_str) == Some("Ready")
                    && c.get("status").and_then(Value::as_str) == Some("True")
            })
        })
}

async fn is_tls_secret_populated(
    client: &Client,
    ns: &str,
    secret_name: &str,
) -> Result<bool, OperatorError> {
    let api: Api<Secret> = Api::namespaced(client.clone(), ns);
    let Some(secret) = api
        .get_opt(secret_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get tenant TLS Secret",
            source,
        })?
    else {
        return Ok(false);
    };
    let Some(data) = secret.data else {
        return Ok(false);
    };
    let has_crt = data.get("tls.crt").is_some_and(|b| !b.0.is_empty());
    let has_key = data.get("tls.key").is_some_and(|b| !b.0.is_empty());
    Ok(has_crt && has_key)
}

fn listener_owners_from_managed_fields(gateway: &DynamicObject) -> BTreeMap<String, Vec<String>> {
    let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Some(entries) = gateway.metadata.managed_fields.as_deref() else {
        return owners;
    };
    for entry in entries {
        let Some(manager) = entry.manager.as_deref() else {
            continue;
        };
        let Some(fields_v1) = entry.fields_v1.as_ref() else {
            continue;
        };
        let Some(listeners_map) = fields_v1
            .0
            .get("f:spec")
            .and_then(|s| s.get("f:listeners"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        for key in listeners_map.keys() {
            if let Some(json_key) = key.strip_prefix("k:") {
                if let Ok(parsed) = serde_json::from_str::<Value>(json_key) {
                    if let Some(listener_name) = parsed.get("name").and_then(Value::as_str) {
                        owners
                            .entry(listener_name.to_owned())
                            .or_default()
                            .push(manager.to_owned());
                    }
                }
            }
        }
    }
    owners
}

fn gateway_has_field_manager(gateway: &DynamicObject, field_manager: &str) -> bool {
    gateway
        .metadata
        .managed_fields
        .as_deref()
        .is_some_and(|entries| {
            entries
                .iter()
                .any(|e| e.manager.as_deref() == Some(field_manager))
        })
}

fn gateway_has_byo_listener_for_hosts(gateway: &DynamicObject, hostnames: &[String]) -> bool {
    let expected: BTreeSet<String> = hostnames.iter().map(|h| byo_listener_name(h)).collect();
    gateway
        .data
        .get("spec")
        .and_then(|s| s.get("listeners"))
        .and_then(Value::as_array)
        .is_some_and(|listeners| {
            listeners.iter().any(|l| {
                l.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| expected.contains(name))
            })
        })
}

struct DynamicApplySpec<'a> {
    namespace: &'a str,
    api_resource: &'a ApiResource,
    name: &'a str,
    manifest: &'a Value,
    field_manager: &'a str,
    context: &'static str,
}

async fn apply_dynamic(
    client: &Client,
    leadership: &LeadershipFence,
    spec: DynamicApplySpec<'_>,
) -> Result<(), OperatorError> {
    leadership.ensure_valid()?;
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), spec.namespace, spec.api_resource);
    let params = PatchParams::apply(spec.field_manager).force();
    api.patch(spec.name, &params, &Patch::Apply(spec.manifest))
        .await
        .map_err(|source| OperatorError::Kube {
            context: spec.context,
            source,
        })?;
    Ok(())
}

async fn delete_dynamic_if_owned(
    client: &Client,
    ns: &str,
    ar: &ApiResource,
    name: &str,
    owner_uid: Option<&str>,
    leadership: &LeadershipFence,
    context: &'static str,
) -> Result<(), OperatorError> {
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), ns, ar);
    let existing = match api.get_opt(name).await {
        Ok(obj) => obj,
        Err(kube::Error::Api(status)) if status.code == 404 => return Ok(()),
        Err(source) => return Err(OperatorError::Kube { context, source }),
    };
    let Some(obj) = existing else {
        return Ok(());
    };
    let owned = owner_uid.is_none_or(|uid| obj.owner_references().iter().any(|o| o.uid == uid));
    if !owned {
        return Ok(());
    }
    leadership.ensure_valid()?;
    let params = DeleteParams {
        preconditions: obj.uid().map(|uid| Preconditions {
            uid: Some(uid),
            resource_version: None,
        }),
        ..DeleteParams::default()
    };
    match api.delete(name, &params).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(status)) if status.code == 404 => Ok(()),
        Err(source) => Err(OperatorError::Kube { context, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_owner() -> OwnerReference {
        OwnerReference {
            api_version: "waddle.social/v1alpha1".to_owned(),
            kind: "WaddleTenant".to_owned(),
            name: "waddle-rawkode-chat".to_owned(),
            uid: "uid-tenant-1".to_owned(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        }
    }

    #[test]
    fn reference_grant_scopes_to_exact_secret_name_and_never_wildcard() {
        let labels = tenant_ingress_labels("waddle", "waddle.rawkode.chat");
        let grant = build_reference_grant_manifest(
            "waddle-tenant-waddle-rawkode-chat-tls-grant",
            "waddle-next",
            "default",
            "waddle-tenant-waddle-rawkode-chat-tls",
            &labels,
            &sample_owner(),
        );
        assert_eq!(grant["apiVersion"], "gateway.networking.k8s.io/v1beta1");
        assert_eq!(grant["kind"], "ReferenceGrant");
        assert_eq!(
            grant["spec"]["from"],
            json!([{
                "group": "gateway.networking.k8s.io",
                "kind": "Gateway",
                "namespace": "default",
            }])
        );
        assert_eq!(
            grant["spec"]["to"],
            json!([{
                "group": "",
                "kind": "Secret",
                "name": "waddle-tenant-waddle-rawkode-chat-tls",
            }])
        );
        assert_eq!(
            grant["metadata"]["ownerReferences"][0]["uid"],
            "uid-tenant-1"
        );
    }

    #[test]
    fn gateway_ssa_payload_and_empty_deletion_payload_are_exact() {
        let listener = ByoListenerSpec {
            name: byo_listener_name("waddle.rawkode.chat"),
            hostname: "waddle.rawkode.chat".to_owned(),
            secret_name: "waddle-tenant-waddle-rawkode-chat-tls".to_owned(),
            secret_namespace: "waddle-next".to_owned(),
        };
        assert!(listener.name.starts_with("byo-"));
        assert_eq!(listener.name.len(), 16);

        let patch =
            build_gateway_ssa_patch("waddle-gateway", "default", &[listener.to_listener_json()]);
        assert_eq!(patch["apiVersion"], "gateway.networking.k8s.io/v1");
        assert_eq!(patch["kind"], "Gateway");
        assert_eq!(patch["metadata"]["name"], "waddle-gateway");
        assert_eq!(patch["metadata"]["namespace"], "default");
        assert_eq!(patch["spec"]["listeners"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            patch["spec"]["listeners"][0]["tls"]["certificateRefs"][0],
            json!({
                "group": "",
                "kind": "Secret",
                "name": "waddle-tenant-waddle-rawkode-chat-tls",
                "namespace": "waddle-next",
            })
        );

        let empty = build_gateway_ssa_patch("waddle-gateway", "default", &[]);
        assert_eq!(empty["spec"]["listeners"], json!([]));
    }

    #[test]
    fn gateway_collision_and_capacity_checks_reject_unsafe_patches() {
        let gw_json = json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "Gateway",
            "metadata": {
                "name": "waddle-gateway",
                "namespace": "default",
                "managedFields": [
                    {
                        "manager": "kustomize-controller",
                        "operation": "Apply",
                        "fieldsV1": {
                            "f:spec": {
                                "f:listeners": {
                                    "k:{\"name\":\"https-xmpp\"}": {}
                                }
                            }
                        }
                    },
                    {
                        "manager": "waddle-operator-tenant.waddle-next.other-tenant",
                        "operation": "Apply",
                        "fieldsV1": {
                            "f:spec": {
                                "f:listeners": {
                                    format!("k:{{\"name\":\"{}\"}}", byo_listener_name("taken.example.com")): {}
                                }
                            }
                        }
                    }
                ]
            },
            "spec": {
                "listeners": [
                    { "name": "https-xmpp", "port": 443, "protocol": "HTTPS", "hostname": "xmpp.waddle.social" },
                    { "name": byo_listener_name("taken.example.com"), "port": 443, "protocol": "HTTPS", "hostname": "taken.example.com" }
                ]
            }
        });
        let gw: DynamicObject = serde_json::from_value(gw_json).expect("valid gateway");
        let our_mgr = tenant_gateway_field_manager("waddle-next", "waddle-rawkode-chat");

        // Static listener hostname collision is rejected.
        let collide_static = ByoListenerSpec {
            name: byo_listener_name("xmpp.waddle.social"),
            hostname: "xmpp.waddle.social".to_owned(),
            secret_name: "s".to_owned(),
            secret_namespace: "waddle-next".to_owned(),
        };
        assert!(matches!(
            check_gateway_listener_conflicts_and_capacity(&gw, &[collide_static], &our_mgr),
            Err(GatewayListenerRejection::HostnameCollision {
                ref existing_listener,
                ..
            }) if existing_listener == "https-xmpp"
        ));

        // Another tenant's BYO listener collision is rejected.
        let collide_other_tenant = ByoListenerSpec {
            name: byo_listener_name("taken.example.com"),
            hostname: "taken.example.com".to_owned(),
            secret_name: "s".to_owned(),
            secret_namespace: "waddle-next".to_owned(),
        };
        assert!(matches!(
            check_gateway_listener_conflicts_and_capacity(&gw, &[collide_other_tenant], &our_mgr),
            Err(GatewayListenerRejection::HostnameCollision { .. })
        ));

        // Same tenant re-applying its own listener succeeds.
        let same_owner = tenant_gateway_field_manager("waddle-next", "other-tenant");
        let own_listener = ByoListenerSpec {
            name: byo_listener_name("taken.example.com"),
            hostname: "taken.example.com".to_owned(),
            secret_name: "s".to_owned(),
            secret_namespace: "waddle-next".to_owned(),
        };
        assert!(
            check_gateway_listener_conflicts_and_capacity(&gw, &[own_listener], &same_owner)
                .is_ok()
        );

        // 64 existing other listeners -> capacity exceeded.
        let full_listeners: Vec<Value> = (0..MAX_GATEWAY_LISTENERS)
            .map(|i| json!({ "name": format!("l-{i}"), "port": 443, "protocol": "HTTPS", "hostname": format!("h{i}.example.com") }))
            .collect();
        let full_gw: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "gateway.networking.k8s.io/v1",
            "kind": "Gateway",
            "metadata": { "name": "waddle-gateway", "namespace": "default" },
            "spec": { "listeners": full_listeners }
        }))
        .expect("full gw");
        let new_listener = ByoListenerSpec {
            name: byo_listener_name("waddle.rawkode.chat"),
            hostname: "waddle.rawkode.chat".to_owned(),
            secret_name: "s".to_owned(),
            secret_namespace: "waddle-next".to_owned(),
        };
        assert!(matches!(
            check_gateway_listener_conflicts_and_capacity(&full_gw, &[new_listener], &our_mgr),
            Err(GatewayListenerRejection::CapacityExceeded { .. })
        ));
    }
}
