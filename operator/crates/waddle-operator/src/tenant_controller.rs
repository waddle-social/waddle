use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition as KubeCondition, Time};
use kube::api::{
    Api, DeleteParams, ListParams, ObjectMeta, Patch, PatchParams, PostParams, Preconditions,
};
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType, Recorder};
use kube::{Client, Resource, ResourceExt};
use serde::Serialize;
use serde_json::json;
use waddle_operator_crd::{
    validate_tenant_ingress_against_server, validate_tenant_spec, AuthzBackendKind, SecretKeyRef,
    TenantExtensionsSpec, TenantPlacementMode, TenantQuotasSpec, WaddleServer, WaddleServerPhase,
    WaddleTenant, WaddleTenantPhase, WaddleTenantStatus,
};

use crate::error::OperatorError;
use crate::leader_election::LeadershipFence;
use crate::metrics::OperatorMetrics;
use crate::openfga_client::OpenFgaClient;
use crate::resources;
use crate::tenant_ingress::{
    cleanup_tenant_ingress, reconcile_tenant_ingress, IngressReconcileOutcome,
};

/// Finalizer attached to `WaddleTenant` resources for projection & aggregator cleanup.
pub const TENANT_FINALIZER: &str = "waddle.social/tenant-cleanup";

const FIELD_MANAGER_TENANT: &str = "waddle-operator-tenant";
const FIELD_MANAGER_TENANT_STATUS: &str = "waddle-operator-tenant-status";
const PENDING_REQUEUE_SECS: u64 = 10;
const DEGRADED_REQUEUE_SECS: u64 = 15;
const READY_REQUEUE_SECS: u64 = 300;

/// Shared dependencies injected into every `WaddleTenant` reconciliation.
#[derive(Clone)]
pub struct TenantControllerContext {
    /// Kubernetes API client.
    pub client: Client,
    /// Shared Prometheus metrics registry.
    pub metrics: Arc<OperatorMetrics>,
    /// Optional Kubernetes event recorder.
    pub recorder: Option<Recorder>,
    /// Monotonic leader lease fence checked prior to mutating cluster state.
    pub leadership: LeadershipFence,
    /// Optional OpenFGA base URL override (used in offline tests).
    pub openfga_endpoint_override: Option<String>,
}

/// JSON document written into `waddle-tenant-<cr-name>` and `<server>-tenants` ConfigMaps.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TenantProjectionSpec {
    /// Canonical tenant DNS domain.
    pub tenant_id: String,
    /// Hosting `WaddleServer` name.
    pub server_name: String,
    /// Tenant placement mode.
    pub placement_mode: TenantPlacementMode,
    /// Tenant owner localparts.
    pub owners: Vec<String>,
    /// Provisioned OpenFGA store ID (when OpenFGA backend is active).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openfga_store_id: Option<String>,
    /// Provisioned immutable OpenFGA authorization model ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openfga_model_id: Option<String>,
    /// Optional reference to per-tenant auth providers Secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_providers_secret_ref: Option<SecretKeyRef>,
    /// Optional tenant quotas.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quotas: Option<TenantQuotasSpec>,
    /// Optional tenant extension policy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<TenantExtensionsSpec>,
    /// Authoritative hostnames routed to this tenant (omitted when empty; defaults to `[tenantId]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hostnames: Vec<String>,
}

/// Reconciles a single `WaddleTenant` resource:
/// 1. Validates spec and ensures `waddle.social/tenant-cleanup` finalizer.
/// 2. Waits for the referenced `WaddleServer` in the same namespace to reach `Ready`.
/// 3. Idempotently provisions OpenFGA store & Waddle authorization model when `WaddleServer` uses OpenFGA.
/// 4. Reconciles `waddle-tenant-<cr-name>` projection ConfigMap and syncs `<server>-tenants`.
/// 5. Patches `WaddleTenant.status` (diff-gated).
///
/// # Errors
/// Returns [`OperatorError`] on leadership loss or unexpected Kubernetes API failures.
pub async fn reconcile_tenant(
    wt: Arc<WaddleTenant>,
    ctx: Arc<TenantControllerContext>,
) -> Result<Action, OperatorError> {
    ctx.leadership.ensure_valid()?;
    ctx.metrics.record_reconcile();

    let cr_name = resources::tenant_name(&wt)?;
    let ns = resources::tenant_namespace(&wt)?;
    let generation = wt.metadata.generation.unwrap_or(1);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    if wt.metadata.deletion_timestamp.is_some() {
        return handle_tenant_deletion(&wt, cr_name, ns, generation, &now, &ctx).await;
    }

    if let Err(report) = validate_tenant_spec(&wt.spec) {
        let existing = wt.status.as_ref();
        let status = build_tenant_status(
            TenantStatusFields {
                phase: WaddleTenantPhase::Degraded,
                observed_generation: generation,
                openfga_store_id: existing.and_then(|s| s.openfga_store_id.clone()),
                openfga_model_id: existing.and_then(|s| s.openfga_model_id.clone()),
                lease_epoch: existing.and_then(|s| s.lease_epoch),
                hostnames: existing.map_or_else(Vec::new, |s| s.hostnames.clone()),
            },
            vec![
                make_condition(
                    "Ready",
                    "False",
                    "SpecInvalid",
                    &report.summary(),
                    generation,
                    &now,
                    existing,
                ),
                make_condition(
                    "SpecValid",
                    "False",
                    "SpecInvalid",
                    &report.summary(),
                    generation,
                    &now,
                    existing,
                ),
            ],
        )?;
        patch_tenant_status_if_changed(&ctx.client, &wt, &status, &ctx.leadership).await?;
        return Ok(Action::requeue(Duration::from_secs(DEGRADED_REQUEUE_SECS)));
    }

    ensure_tenant_finalizer(&ctx.client, &wt, &ctx.leadership).await?;

    let server_name = wt.spec.server_ref.name.as_str();
    let server_api: Api<WaddleServer> = Api::namespaced(ctx.client.clone(), ns);
    let Some(ws) = server_api
        .get_opt(server_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get referenced WaddleServer",
            source,
        })?
    else {
        return set_pending_status(
            &wt,
            generation,
            &now,
            "ServerNotFound",
            &format!("WaddleServer {server_name} not found in namespace {ns}"),
            &ctx,
        )
        .await;
    };

    let server_phase = ws.status.as_ref().map(|s| s.phase);
    if server_phase != Some(WaddleServerPhase::Ready) {
        let phase_label = server_phase.map_or("Initializing", |p| match p {
            WaddleServerPhase::Initializing => "Initializing",
            WaddleServerPhase::Pending => "Pending",
            WaddleServerPhase::Provisioning => "Provisioning",
            WaddleServerPhase::Reconciling => "Reconciling",
            WaddleServerPhase::Ready => "Ready",
            WaddleServerPhase::Degraded => "Degraded",
            WaddleServerPhase::Failed => "Failed",
            WaddleServerPhase::Terminating => "Terminating",
        });
        return set_pending_status(
            &wt,
            generation,
            &now,
            "ServerNotReady",
            &format!(
                "Waiting for WaddleServer {server_name} to reach Ready (currently {phase_label})"
            ),
            &ctx,
        )
        .await;
    }

    let existing_status = wt.status.as_ref();
    let mut store_id = existing_status
        .and_then(|s| s.openfga_store_id.clone())
        .filter(|v| !v.trim().is_empty());
    let mut model_id = existing_status
        .and_then(|s| s.openfga_model_id.clone())
        .filter(|v| !v.trim().is_empty());

    let resolved_ingress = match validate_tenant_ingress_against_server(&wt.spec, &ws.spec.ingress)
    {
        Ok(resolved) => resolved,
        Err(err) => {
            let msg = err.to_string();
            let degraded = build_tenant_status(
                TenantStatusFields {
                    phase: WaddleTenantPhase::Degraded,
                    observed_generation: generation,
                    openfga_store_id: store_id,
                    openfga_model_id: model_id,
                    lease_epoch: existing_status.and_then(|s| s.lease_epoch),
                    hostnames: existing_status.map_or_else(Vec::new, |s| s.hostnames.clone()),
                },
                vec![
                    make_condition(
                        "Ready",
                        "False",
                        "IngressInvalid",
                        &msg,
                        generation,
                        &now,
                        existing_status,
                    ),
                    make_condition(
                        "SpecValid",
                        "True",
                        "SpecValid",
                        "WaddleTenant specification is valid",
                        generation,
                        &now,
                        existing_status,
                    ),
                    make_condition(
                        "ServerReady",
                        "True",
                        "Ready",
                        &format!("WaddleServer {server_name} is Ready"),
                        generation,
                        &now,
                        existing_status,
                    ),
                    make_condition(
                        "IngressReady",
                        "False",
                        "IngressInvalid",
                        &msg,
                        generation,
                        &now,
                        existing_status,
                    ),
                ],
            )?;
            patch_tenant_status_if_changed(&ctx.client, &wt, &degraded, &ctx.leadership).await?;
            publish_tenant_event(
                ctx.recorder.as_ref(),
                &wt,
                EventType::Warning,
                "IngressInvalid",
                &msg,
            )
            .await;
            return Ok(Action::requeue(Duration::from_secs(DEGRADED_REQUEUE_SECS)));
        }
    };

    if let Some((conflict_host, other_tenant)) = check_namespace_hostname_conflicts(
        &ctx.client,
        ns,
        cr_name,
        server_name,
        &resolved_ingress.hostnames,
    )
    .await?
    {
        let msg = format!(
            "Hostname '{conflict_host}' is already claimed by WaddleTenant '{other_tenant}' on server '{server_name}'"
        );
        let degraded = build_tenant_status(
            TenantStatusFields {
                phase: WaddleTenantPhase::Degraded,
                observed_generation: generation,
                openfga_store_id: store_id,
                openfga_model_id: model_id,
                lease_epoch: existing_status.and_then(|s| s.lease_epoch),
                hostnames: existing_status.map_or_else(Vec::new, |s| s.hostnames.clone()),
            },
            vec![
                make_condition(
                    "Ready",
                    "False",
                    "HostnameConflict",
                    &msg,
                    generation,
                    &now,
                    existing_status,
                ),
                make_condition(
                    "SpecValid",
                    "True",
                    "SpecValid",
                    "WaddleTenant specification is valid",
                    generation,
                    &now,
                    existing_status,
                ),
                make_condition(
                    "ServerReady",
                    "True",
                    "Ready",
                    &format!("WaddleServer {server_name} is Ready"),
                    generation,
                    &now,
                    existing_status,
                ),
                make_condition(
                    "IngressReady",
                    "False",
                    "HostnameConflict",
                    &msg,
                    generation,
                    &now,
                    existing_status,
                ),
            ],
        )?;
        patch_tenant_status_if_changed(&ctx.client, &wt, &degraded, &ctx.leadership).await?;
        publish_tenant_event(
            ctx.recorder.as_ref(),
            &wt,
            EventType::Warning,
            "HostnameConflict",
            &msg,
        )
        .await;
        return Ok(Action::requeue(Duration::from_secs(DEGRADED_REQUEUE_SECS)));
    }

    let openfga_condition = if ws.spec.authz.backend == AuthzBackendKind::OpenFga {
        match provision_tenant_openfga(
            &ctx,
            &ws,
            ns,
            &wt.spec.tenant_id,
            &mut store_id,
            &mut model_id,
        )
        .await
        {
            Ok(()) => make_condition(
                "OpenFgaReady",
                "True",
                "Provisioned",
                "Tenant OpenFGA store and authorization model are provisioned",
                generation,
                &now,
                existing_status,
            ),
            Err(err) => {
                let msg = err.to_string();
                let degraded = build_tenant_status(
                    TenantStatusFields {
                        phase: WaddleTenantPhase::Degraded,
                        observed_generation: generation,
                        openfga_store_id: store_id,
                        openfga_model_id: model_id,
                        lease_epoch: existing_status.and_then(|s| s.lease_epoch),
                        hostnames: resolved_ingress.hostnames,
                    },
                    vec![
                        make_condition(
                            "Ready",
                            "False",
                            "OpenFgaProvisionFailed",
                            &msg,
                            generation,
                            &now,
                            existing_status,
                        ),
                        make_condition(
                            "SpecValid",
                            "True",
                            "SpecValid",
                            "WaddleTenant specification is valid",
                            generation,
                            &now,
                            existing_status,
                        ),
                        make_condition(
                            "ServerReady",
                            "True",
                            "Ready",
                            &format!("WaddleServer {server_name} is Ready"),
                            generation,
                            &now,
                            existing_status,
                        ),
                        make_condition(
                            "OpenFgaReady",
                            "False",
                            "OpenFgaProvisionFailed",
                            &msg,
                            generation,
                            &now,
                            existing_status,
                        ),
                    ],
                )?;
                patch_tenant_status_if_changed(&ctx.client, &wt, &degraded, &ctx.leadership)
                    .await?;
                publish_tenant_event(
                    ctx.recorder.as_ref(),
                    &wt,
                    EventType::Warning,
                    "OpenFgaProvisionFailed",
                    &msg,
                )
                .await;
                return Ok(Action::requeue(Duration::from_secs(DEGRADED_REQUEUE_SECS)));
            }
        }
    } else {
        make_condition(
            "OpenFgaReady",
            "True",
            "NotRequired",
            "Referenced WaddleServer does not use OpenFGA",
            generation,
            &now,
            existing_status,
        )
    };

    let projection = TenantProjectionSpec {
        tenant_id: wt.spec.tenant_id.clone(),
        server_name: server_name.to_owned(),
        placement_mode: wt.spec.placement_mode,
        owners: wt.spec.owners.clone(),
        openfga_store_id: store_id.clone(),
        openfga_model_id: model_id.clone(),
        auth_providers_secret_ref: wt.spec.auth_providers_secret_ref.clone(),
        quotas: wt.spec.quotas.clone(),
        extensions: wt.spec.extensions.clone(),
        hostnames: resolved_ingress.hostnames.clone(),
    };
    let projection_json = serde_json::to_string_pretty(&projection)
        .map_err(|err| OperatorError::Serialization(err.to_string()))?;

    reconcile_projection_config_map(&ctx.client, ns, &wt, &projection_json, &ctx.leadership)
        .await?;
    sync_server_tenants_config_map(
        &ctx.client,
        ns,
        server_name,
        &wt.spec.tenant_id,
        Some(&projection_json),
        &ctx.leadership,
    )
    .await?;

    let projection_condition = make_condition(
        "ProjectionReady",
        "True",
        "Synced",
        &format!(
            "Synced ConfigMap {} and {}-tenants",
            resources::tenant_projection_config_map_name(cr_name),
            server_name
        ),
        generation,
        &now,
        existing_status,
    );

    let ingress_outcome = reconcile_tenant_ingress(
        &ctx.client,
        ns,
        &ws,
        &wt,
        &resolved_ingress,
        &ctx.leadership,
    )
    .await?;

    let lease_epoch = Some(existing_status.and_then(|s| s.lease_epoch).unwrap_or(1));
    let (phase, requeue_secs, ready_cond, tls_cond, ingress_cond, warning_event) =
        match ingress_outcome {
            IngressReconcileOutcome::Disabled => (
                WaddleTenantPhase::Ready,
                READY_REQUEUE_SECS,
                make_condition(
                    "Ready",
                    "True",
                    "Ready",
                    "Tenant OpenFGA store and projection ConfigMap are ready",
                    generation,
                    &now,
                    existing_status,
                ),
                make_condition(
                    "TlsReady",
                    "True",
                    "IngressDisabled",
                    "Tenant ingress management is disabled",
                    generation,
                    &now,
                    existing_status,
                ),
                make_condition(
                    "IngressReady",
                    "True",
                    "IngressDisabled",
                    "Tenant ingress management is disabled",
                    generation,
                    &now,
                    existing_status,
                ),
                None,
            ),
            IngressReconcileOutcome::Ready {
                tls_reason,
                tls_message,
                ingress_message,
            } => (
                WaddleTenantPhase::Ready,
                READY_REQUEUE_SECS,
                make_condition(
                    "Ready",
                    "True",
                    "Ready",
                    "Tenant OpenFGA store, projection ConfigMap, TLS, and ingress are ready",
                    generation,
                    &now,
                    existing_status,
                ),
                make_condition(
                    "TlsReady",
                    "True",
                    tls_reason,
                    &tls_message,
                    generation,
                    &now,
                    existing_status,
                ),
                make_condition(
                    "IngressReady",
                    "True",
                    "Reconciled",
                    &ingress_message,
                    generation,
                    &now,
                    existing_status,
                ),
                None,
            ),
            IngressReconcileOutcome::WaitingForCertificate { message } => (
                WaddleTenantPhase::Provisioning,
                PENDING_REQUEUE_SECS,
                make_condition(
                    "Ready",
                    "False",
                    "WaitingForCertificate",
                    &message,
                    generation,
                    &now,
                    existing_status,
                ),
                make_condition(
                    "TlsReady",
                    "False",
                    "CertificateNotReady",
                    &message,
                    generation,
                    &now,
                    existing_status,
                ),
                make_condition(
                    "IngressReady",
                    "False",
                    "WaitingForCertificate",
                    &message,
                    generation,
                    &now,
                    existing_status,
                ),
                None,
            ),
            IngressReconcileOutcome::Degraded { reason, message } => (
                WaddleTenantPhase::Degraded,
                DEGRADED_REQUEUE_SECS,
                make_condition(
                    "Ready",
                    "False",
                    reason,
                    &message,
                    generation,
                    &now,
                    existing_status,
                ),
                make_condition(
                    "TlsReady",
                    "False",
                    reason,
                    &message,
                    generation,
                    &now,
                    existing_status,
                ),
                make_condition(
                    "IngressReady",
                    "False",
                    reason,
                    &message,
                    generation,
                    &now,
                    existing_status,
                ),
                Some((reason, message)),
            ),
        };

    let status = build_tenant_status(
        TenantStatusFields {
            phase,
            observed_generation: generation,
            openfga_store_id: store_id,
            openfga_model_id: model_id,
            lease_epoch,
            hostnames: resolved_ingress.hostnames,
        },
        vec![
            ready_cond,
            make_condition(
                "SpecValid",
                "True",
                "SpecValid",
                "WaddleTenant specification is valid",
                generation,
                &now,
                existing_status,
            ),
            make_condition(
                "ServerReady",
                "True",
                "Ready",
                &format!("WaddleServer {server_name} is Ready"),
                generation,
                &now,
                existing_status,
            ),
            openfga_condition,
            projection_condition,
            tls_cond,
            ingress_cond,
        ],
    )?;

    let patched =
        patch_tenant_status_if_changed(&ctx.client, &wt, &status, &ctx.leadership).await?;
    if let Some((reason, message)) = warning_event {
        publish_tenant_event(
            ctx.recorder.as_ref(),
            &wt,
            EventType::Warning,
            reason,
            &message,
        )
        .await;
    } else if patched && phase == WaddleTenantPhase::Ready {
        publish_tenant_event(
            ctx.recorder.as_ref(),
            &wt,
            EventType::Normal,
            "TenantReady",
            &format!(
                "WaddleTenant {} is Ready on server {server_name}",
                wt.spec.tenant_id
            ),
        )
        .await;
    }

    Ok(Action::requeue(Duration::from_secs(requeue_secs)))
}

async fn check_namespace_hostname_conflicts(
    client: &Client,
    ns: &str,
    cr_name: &str,
    server_name: &str,
    desired_hostnames: &[String],
) -> Result<Option<(String, String)>, OperatorError> {
    let desired_lower: BTreeSet<String> = desired_hostnames
        .iter()
        .map(|h| h.to_ascii_lowercase())
        .collect();
    if desired_lower.is_empty() {
        return Ok(None);
    }

    let api: Api<WaddleTenant> = Api::namespaced(client.clone(), ns);
    let tenants = api
        .list(&ListParams::default())
        .await
        .map_err(|source| OperatorError::Kube {
            context: "list WaddleTenants for hostname uniqueness check",
            source,
        })?;

    for other in tenants.items {
        let Some(other_name) = other.metadata.name.as_deref() else {
            continue;
        };
        if other_name == cr_name || other.metadata.deletion_timestamp.is_some() {
            continue;
        }
        if other.spec.server_ref.name != server_name {
            continue;
        }
        for other_host in other.spec.effective_hostnames() {
            let lower = other_host.to_ascii_lowercase();
            if desired_lower.contains(&lower) {
                return Ok(Some((lower, other_name.to_owned())));
            }
        }
    }

    Ok(None)
}

async fn handle_tenant_deletion(
    wt: &WaddleTenant,
    cr_name: &str,
    ns: &str,
    generation: i64,
    now: &str,
    ctx: &TenantControllerContext,
) -> Result<Action, OperatorError> {
    let existing = wt.status.as_ref();
    let term_status = build_tenant_status(
        TenantStatusFields {
            phase: WaddleTenantPhase::Terminating,
            observed_generation: generation,
            openfga_store_id: existing.and_then(|s| s.openfga_store_id.clone()),
            openfga_model_id: existing.and_then(|s| s.openfga_model_id.clone()),
            lease_epoch: existing.and_then(|s| s.lease_epoch),
            hostnames: existing.map_or_else(Vec::new, |s| s.hostnames.clone()),
        },
        vec![make_condition(
            "Ready",
            "False",
            "Terminating",
            "WaddleTenant is terminating and cleaning up ingress and projection resources",
            generation,
            now,
            existing,
        )],
    )?;
    patch_tenant_status_if_changed(&ctx.client, wt, &term_status, &ctx.leadership).await?;

    let server_name = wt.spec.server_ref.name.as_str();
    let server_api: Api<WaddleServer> = Api::namespaced(ctx.client.clone(), ns);
    let ws = server_api
        .get_opt(server_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get referenced WaddleServer during tenant deletion",
            source,
        })?;
    cleanup_tenant_ingress(&ctx.client, ns, cr_name, wt, ws.as_ref(), &ctx.leadership).await?;

    sync_server_tenants_config_map(
        &ctx.client,
        ns,
        server_name,
        &wt.spec.tenant_id,
        None,
        &ctx.leadership,
    )
    .await?;

    let cm_api: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), ns);
    let proj_name = resources::tenant_projection_config_map_name(cr_name);
    if let Some(existing_cm) =
        cm_api
            .get_opt(&proj_name)
            .await
            .map_err(|source| OperatorError::Kube {
                context: "get tenant projection ConfigMap for cleanup",
                source,
            })?
    {
        let owner_uid = wt.metadata.uid.as_deref();
        let owned =
            owner_uid.is_none_or(|uid| existing_cm.owner_references().iter().any(|o| o.uid == uid));
        if owned {
            ctx.leadership.ensure_valid()?;
            let params = DeleteParams {
                preconditions: existing_cm.uid().map(|uid| Preconditions {
                    uid: Some(uid),
                    resource_version: None,
                }),
                ..DeleteParams::default()
            };
            match cm_api.delete(&proj_name, &params).await {
                Ok(_) => {}
                Err(kube::Error::Api(status)) if status.code == 404 => {}
                Err(source) => {
                    return Err(OperatorError::Kube {
                        context: "delete tenant projection ConfigMap",
                        source,
                    });
                }
            }
        }
    }

    remove_tenant_finalizer(&ctx.client, wt, &ctx.leadership).await?;
    Ok(Action::await_change())
}

async fn set_pending_status(
    wt: &WaddleTenant,
    generation: i64,
    now: &str,
    reason: &str,
    message: &str,
    ctx: &TenantControllerContext,
) -> Result<Action, OperatorError> {
    let existing = wt.status.as_ref();
    let status = build_tenant_status(
        TenantStatusFields {
            phase: WaddleTenantPhase::Pending,
            observed_generation: generation,
            openfga_store_id: existing.and_then(|s| s.openfga_store_id.clone()),
            openfga_model_id: existing.and_then(|s| s.openfga_model_id.clone()),
            lease_epoch: existing.and_then(|s| s.lease_epoch),
            hostnames: existing.map_or_else(Vec::new, |s| s.hostnames.clone()),
        },
        vec![
            make_condition("Ready", "False", reason, message, generation, now, existing),
            make_condition(
                "SpecValid",
                "True",
                "SpecValid",
                "WaddleTenant specification is valid",
                generation,
                now,
                existing,
            ),
            make_condition(
                "ServerReady",
                "False",
                reason,
                message,
                generation,
                now,
                existing,
            ),
        ],
    )?;
    patch_tenant_status_if_changed(&ctx.client, wt, &status, &ctx.leadership).await?;
    Ok(Action::requeue(Duration::from_secs(PENDING_REQUEUE_SECS)))
}

async fn provision_tenant_openfga(
    ctx: &TenantControllerContext,
    ws: &WaddleServer,
    ns: &str,
    tenant_id: &str,
    store_id: &mut Option<String>,
    model_id: &mut Option<String>,
) -> Result<(), OperatorError> {
    if store_id.is_some() && model_id.is_some() {
        return Ok(());
    }

    let openfga = ws.spec.authz.openfga.as_ref().ok_or_else(|| {
        OperatorError::InvalidClusterState(
            "WaddleServer authz.backend is OpenFga without spec.authz.openfga".to_owned(),
        )
    })?;

    let endpoint = ctx.openfga_endpoint_override.clone().unwrap_or_else(|| {
        format!(
            "{}://{}.{}.svc:{}",
            openfga.scheme, openfga.service_name, ns, openfga.port
        )
    });

    let bearer_token = if let Some(sec_ref) = &openfga.auth_secret_ref {
        Some(read_secret_key_utf8(&ctx.client, ns, &sec_ref.name, &sec_ref.key).await?)
    } else {
        None
    };

    let fga_client = OpenFgaClient::new(endpoint, bearer_token)?;
    let resolved_store_id = match store_id.clone() {
        Some(id) => id,
        None => {
            let store_name = format!("waddle-tenant-{tenant_id}");
            let id = fga_client.ensure_store(&store_name).await?;
            *store_id = Some(id.clone());
            id
        }
    };

    if model_id.is_none() {
        let written_model_id = fga_client
            .ensure_waddle_authorization_model(&resolved_store_id)
            .await?;
        *model_id = Some(written_model_id);
    }

    Ok(())
}

async fn read_secret_key_utf8(
    client: &Client,
    ns: &str,
    secret_name: &str,
    key: &str,
) -> Result<String, OperatorError> {
    let secret = Api::<Secret>::namespaced(client.clone(), ns)
        .get_opt(secret_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get OpenFGA auth Secret for tenant provisioning",
            source,
        })?
        .ok_or_else(|| OperatorError::OpenFga {
            context: "load OpenFGA auth token",
            detail: format!("Secret {secret_name} not found in namespace {ns}"),
        })?;
    let bytes = secret
        .data
        .and_then(|mut data| data.remove(key))
        .map(|b| b.0)
        .filter(|raw| !raw.is_empty())
        .ok_or_else(|| OperatorError::OpenFga {
            context: "load OpenFGA auth token",
            detail: format!("Secret {secret_name} lacks non-empty key '{key}'"),
        })?;
    let token = String::from_utf8(bytes).map_err(|_| OperatorError::OpenFga {
        context: "load OpenFGA auth token",
        detail: format!("Secret {secret_name}/{key} is not valid UTF-8"),
    })?;
    let trimmed = token.trim().to_owned();
    if trimmed.is_empty() {
        return Err(OperatorError::OpenFga {
            context: "load OpenFGA auth token",
            detail: format!("Secret {secret_name}/{key} is blank"),
        });
    }
    Ok(trimmed)
}

async fn reconcile_projection_config_map(
    client: &Client,
    ns: &str,
    wt: &WaddleTenant,
    projection_json: &str,
    leadership: &LeadershipFence,
) -> Result<(), OperatorError> {
    let cr_name = resources::tenant_name(wt)?;
    let cm_name = resources::tenant_projection_config_map_name(cr_name);
    let owner = resources::tenant_owner_reference(wt)?;

    let mut labels = BTreeMap::new();
    labels.insert(
        resources::MANAGED_BY_LABEL.to_owned(),
        resources::MANAGED_BY_VALUE.to_owned(),
    );
    labels.insert(
        "app.kubernetes.io/component".to_owned(),
        "tenant".to_owned(),
    );
    labels.insert(
        resources::SERVER_LABEL.to_owned(),
        wt.spec.server_ref.name.clone(),
    );
    labels.insert(
        resources::TENANT_LABEL.to_owned(),
        wt.spec.tenant_id.clone(),
    );

    let mut data = BTreeMap::new();
    data.insert(
        format!("{}.json", wt.spec.tenant_id),
        projection_json.to_owned(),
    );

    let cm = ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name.clone()),
            labels: Some(labels),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        data: Some(data),
        ..ConfigMap::default()
    };

    leadership.ensure_valid()?;
    let cm_api: Api<ConfigMap> = Api::namespaced(client.clone(), ns);
    let params = PatchParams::apply(FIELD_MANAGER_TENANT).force();
    cm_api
        .patch(&cm_name, &params, &Patch::Apply(&cm))
        .await
        .map_err(|source| OperatorError::Kube {
            context: "apply tenant projection ConfigMap",
            source,
        })?;
    Ok(())
}

async fn sync_server_tenants_config_map(
    client: &Client,
    ns: &str,
    server_name: &str,
    tenant_id: &str,
    projection_json: Option<&str>,
    leadership: &LeadershipFence,
) -> Result<(), OperatorError> {
    let cm_name = resources::tenants_config_map_name_for_server(server_name);
    let key = format!("{tenant_id}.json");
    let cm_api: Api<ConfigMap> = Api::namespaced(client.clone(), ns);

    match cm_api
        .get_opt(&cm_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get server aggregated tenants ConfigMap",
            source,
        })? {
        Some(mut cm) => {
            let data = cm.data.get_or_insert_with(BTreeMap::new);
            let changed = match projection_json {
                Some(val) => {
                    if data.get(&key).map(String::as_str) == Some(val) {
                        false
                    } else {
                        data.insert(key, val.to_owned());
                        true
                    }
                }
                None => data.remove(&key).is_some(),
            };
            if changed {
                leadership.ensure_valid()?;
                cm_api
                    .replace(&cm_name, &PostParams::default(), &cm)
                    .await
                    .map_err(|source| OperatorError::Kube {
                        context: "update server aggregated tenants ConfigMap",
                        source,
                    })?;
            }
        }
        None => {
            if let Some(val) = projection_json {
                let mut labels = BTreeMap::new();
                labels.insert(
                    resources::MANAGED_BY_LABEL.to_owned(),
                    resources::MANAGED_BY_VALUE.to_owned(),
                );
                labels.insert(resources::SERVER_LABEL.to_owned(), server_name.to_owned());
                let mut data = BTreeMap::new();
                data.insert(key, val.to_owned());
                let cm = ConfigMap {
                    metadata: ObjectMeta {
                        name: Some(cm_name.clone()),
                        labels: Some(labels),
                        ..ObjectMeta::default()
                    },
                    data: Some(data),
                    ..ConfigMap::default()
                };
                leadership.ensure_valid()?;
                match cm_api.create(&PostParams::default(), &cm).await {
                    Ok(_) => {}
                    Err(kube::Error::Api(status)) if status.code == 409 => {}
                    Err(source) => {
                        return Err(OperatorError::Kube {
                            context: "create server aggregated tenants ConfigMap",
                            source,
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

async fn ensure_tenant_finalizer(
    client: &Client,
    wt: &WaddleTenant,
    leadership: &LeadershipFence,
) -> Result<(), OperatorError> {
    let mut finalizers = wt.metadata.finalizers.clone().unwrap_or_default();
    if finalizers.iter().any(|f| f == TENANT_FINALIZER) {
        return Ok(());
    }
    finalizers.push(TENANT_FINALIZER.to_owned());

    let name = resources::tenant_name(wt)?;
    let ns = resources::tenant_namespace(wt)?;
    leadership.ensure_valid()?;

    let api: Api<WaddleTenant> = Api::namespaced(client.clone(), ns);
    let patch = json!({ "metadata": { "finalizers": finalizers } });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(|source| OperatorError::Kube {
            context: "add finalizer to WaddleTenant",
            source,
        })?;
    Ok(())
}

async fn remove_tenant_finalizer(
    client: &Client,
    wt: &WaddleTenant,
    leadership: &LeadershipFence,
) -> Result<(), OperatorError> {
    let finalizers = wt.metadata.finalizers.clone().unwrap_or_default();
    if !finalizers.iter().any(|f| f == TENANT_FINALIZER) {
        return Ok(());
    }
    let remaining: Vec<String> = finalizers
        .into_iter()
        .filter(|f| f != TENANT_FINALIZER)
        .collect();

    let name = resources::tenant_name(wt)?;
    let ns = resources::tenant_namespace(wt)?;
    leadership.ensure_valid()?;

    let api: Api<WaddleTenant> = Api::namespaced(client.clone(), ns);
    let patch = json!({ "metadata": { "finalizers": remaining } });
    api.patch(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(|source| OperatorError::Kube {
            context: "remove finalizer from WaddleTenant",
            source,
        })?;
    Ok(())
}

async fn patch_tenant_status_if_changed(
    client: &Client,
    wt: &WaddleTenant,
    desired: &WaddleTenantStatus,
    leadership: &LeadershipFence,
) -> Result<bool, OperatorError> {
    if wt.status.as_ref() == Some(desired) {
        return Ok(false);
    }

    let name = resources::tenant_name(wt)?;
    let ns = resources::tenant_namespace(wt)?;
    leadership.ensure_valid()?;

    let api: Api<WaddleTenant> = Api::namespaced(client.clone(), ns);
    let payload = json!({
        "apiVersion": WaddleTenant::api_version(&()),
        "kind": WaddleTenant::kind(&()),
        "status": desired,
    });
    let params = PatchParams::apply(FIELD_MANAGER_TENANT_STATUS).force();
    api.patch_status(name, &params, &Patch::Apply(&payload))
        .await
        .map_err(|source| OperatorError::Kube {
            context: "patch WaddleTenant status",
            source,
        })?;
    Ok(true)
}

struct ConditionDraft {
    kind: &'static str,
    status: &'static str,
    reason: String,
    message: String,
    observed_generation: i64,
    last_transition_time: String,
}

fn make_condition(
    kind: &'static str,
    status: &'static str,
    reason: &str,
    message: &str,
    observed_generation: i64,
    now_rfc3339: &str,
    existing: Option<&WaddleTenantStatus>,
) -> ConditionDraft {
    let last_transition_time = existing
        .and_then(|s| {
            s.conditions
                .iter()
                .find(|c| c.type_ == kind && c.status == status)
        })
        .map_or_else(
            || now_rfc3339.to_owned(),
            |c| {
                chrono::DateTime::<chrono::Utc>::from_str(&c.last_transition_time.0.to_string())
                    .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    .unwrap_or_else(|_| c.last_transition_time.0.to_string())
            },
        );
    ConditionDraft {
        kind,
        status,
        reason: reason.to_owned(),
        message: message.to_owned(),
        observed_generation,
        last_transition_time,
    }
}

struct TenantStatusFields {
    phase: WaddleTenantPhase,
    observed_generation: i64,
    openfga_store_id: Option<String>,
    openfga_model_id: Option<String>,
    lease_epoch: Option<u64>,
    hostnames: Vec<String>,
}

fn build_tenant_status(
    fields: TenantStatusFields,
    drafts: Vec<ConditionDraft>,
) -> Result<WaddleTenantStatus, OperatorError> {
    let conditions = drafts
        .into_iter()
        .map(|draft| {
            let ts = k8s_openapi::jiff::Timestamp::from_str(&draft.last_transition_time).map_err(
                |err| {
                    OperatorError::InvalidClusterState(format!(
                        "invalid tenant condition timestamp '{}': {err}",
                        draft.last_transition_time
                    ))
                },
            )?;
            Ok(KubeCondition {
                type_: draft.kind.to_owned(),
                status: draft.status.to_owned(),
                reason: draft.reason,
                message: draft.message,
                observed_generation: Some(draft.observed_generation),
                last_transition_time: Time(ts),
            })
        })
        .collect::<Result<Vec<_>, OperatorError>>()?;

    Ok(WaddleTenantStatus {
        phase: fields.phase,
        observed_generation: Some(fields.observed_generation),
        openfga_store_id: fields.openfga_store_id,
        openfga_model_id: fields.openfga_model_id,
        lease_epoch: fields.lease_epoch,
        hostnames: fields.hostnames,
        conditions,
    })
}

async fn publish_tenant_event(
    recorder: Option<&Recorder>,
    wt: &WaddleTenant,
    event_type: EventType,
    reason: &str,
    message: &str,
) {
    let Some(rec) = recorder else {
        return;
    };
    let ev = Event {
        type_: event_type,
        reason: reason.to_owned(),
        note: Some(message.chars().take(1000).collect()),
        action: "Reconcile".to_owned(),
        secondary: None,
    };
    let obj_ref = wt.object_ref(&());
    if let Err(err) = rec.publish(&ev, &obj_ref).await {
        tracing::warn!(
            error = %err,
            reason,
            "failed to publish Kubernetes Event for WaddleTenant"
        );
    }
}
