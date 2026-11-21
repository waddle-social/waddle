use std::fmt::Debug;
use std::str::FromStr;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{
    ConfigMap, PersistentVolumeClaim, Secret, Service, ServiceAccount,
};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition as KubeCondition, Time};
use kube::api::{Api, DeleteParams, DynamicObject, Patch, PatchParams, PostParams};
use kube::runtime::events::{Event, EventType, Recorder};
use kube::{Client, Resource};
use serde::Serialize;
use serde_json::json;
use waddle_operator_crd::{
    DependenciesStatus, WaddleServer, WaddleServerPhase, WaddleServerStatus,
};
use waddle_operator_fsm::{
    ConditionRecord, EventSeverity, Phase, ReconcileCommand, TransitionOutcome,
};

use crate::bindings::ResolvedRuntimeBindings;
use crate::error::OperatorError;
use crate::external_crds;
use crate::observer::WorldSnapshot;
use crate::resources;

const FIELD_MANAGER_WORKLOAD: &str = "waddle-operator";
const FIELD_MANAGER_STATUS: &str = "waddle-operator-status";
const MAX_EVENT_NOTE_BYTES: usize = 1000;

/// Executes declarative state machine commands against the Kubernetes API.
///
/// # Errors
/// Returns [`OperatorError`] if any required Kubernetes create, replace, or SSA operation fails.
pub async fn apply_commands(
    client: &Client,
    ws: &WaddleServer,
    snapshot: &WorldSnapshot,
    commands: &[ReconcileCommand],
    recorder: Option<&Recorder>,
) -> Result<(), OperatorError> {
    let ns = resources::workload_namespace(ws)?;

    for command in commands {
        match command {
            ReconcileCommand::EnsureYoloSecret => {
                ensure_yolo_secret(client, ns, ws).await?;
            }
            ReconcileCommand::ApplyManagedExternalSecret => {
                apply_managed_external_secret(client, ns, ws, snapshot.bindings.as_ref()).await?;
            }
            ReconcileCommand::ApplyWorkload => {
                let validated = snapshot.validated.as_ref().ok_or_else(|| {
                    OperatorError::InvalidClusterState(
                        "ApplyWorkload requested without ValidatedSpec".to_owned(),
                    )
                })?;
                let bindings = snapshot.bindings.as_ref().ok_or_else(|| {
                    OperatorError::InvalidClusterState(
                        "ApplyWorkload requested without ResolvedRuntimeBindings".to_owned(),
                    )
                })?;
                apply_workload(client, ns, ws, validated, bindings).await?;
            }
            ReconcileCommand::EmitEvent(notice) => {
                if let Some(rec) = recorder {
                    let ev = Event {
                        type_: match notice.severity {
                            EventSeverity::Normal => EventType::Normal,
                            EventSeverity::Warning => EventType::Warning,
                        },
                        reason: notice.reason.clone(),
                        note: Some(truncate_utf8(&notice.message, MAX_EVENT_NOTE_BYTES)),
                        action: "Reconcile".to_owned(),
                        secondary: None,
                    };
                    let obj_ref = ws.object_ref(&());
                    if let Err(err) = rec.publish(&ev, &obj_ref).await {
                        tracing::warn!(
                            error = %err,
                            reason = %notice.reason,
                            "failed to publish Kubernetes Event for WaddleServer"
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

/// Applies `WaddleServer.status` via SSA only when it differs from the live status subresource.
///
/// Returns `true` if a `patch_status` API call was issued, `false` when already converged.
///
/// # Errors
/// Returns [`OperatorError`] on invalid condition timestamps or Kubernetes API failure.
pub async fn patch_status_if_changed(
    client: &Client,
    ws: &WaddleServer,
    outcome: &TransitionOutcome,
    snapshot: &WorldSnapshot,
) -> Result<bool, OperatorError> {
    let name = resources::workload_name(ws)?;
    let ns = resources::workload_namespace(ws)?;

    let desired_status = build_status(ws, outcome, snapshot)?;
    if ws.status.as_ref() == Some(&desired_status) {
        return Ok(false);
    }

    let api: Api<WaddleServer> = Api::namespaced(client.clone(), ns);
    let payload = json!({
        "apiVersion": WaddleServer::api_version(&()),
        "kind": WaddleServer::kind(&()),
        "status": desired_status,
    });
    let params = PatchParams::apply(FIELD_MANAGER_STATUS).force();
    api.patch_status(name, &params, &Patch::Apply(&payload))
        .await
        .map_err(|source| OperatorError::Kube {
            context: "patch WaddleServer status",
            source,
        })?;

    Ok(true)
}

async fn ensure_yolo_secret(
    client: &Client,
    ns: &str,
    ws: &WaddleServer,
) -> Result<(), OperatorError> {
    let api: Api<Secret> = Api::namespaced(client.clone(), ns);
    let name = resources::yolo_secret_name(ws)?;

    match api
        .get_opt(&name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get YOLO Secret",
            source,
        })? {
        None => {
            let fresh = resources::build_yolo_secret(ws)?;
            match api.create(&PostParams::default(), &fresh).await {
                Ok(_) => Ok(()),
                Err(kube::Error::Api(status)) if status.code == 409 => {
                    let mut existing =
                        api.get(&name).await.map_err(|source| OperatorError::Kube {
                            context: "re-read YOLO Secret after 409 conflict",
                            source,
                        })?;
                    if resources::fill_missing_yolo_keys(ws, &mut existing)? {
                        api.replace(&name, &PostParams::default(), &existing)
                            .await
                            .map_err(|source| OperatorError::Kube {
                                context: "replace YOLO Secret after 409 conflict",
                                source,
                            })?;
                    }
                    Ok(())
                }
                Err(source) => Err(OperatorError::Kube {
                    context: "create YOLO Secret",
                    source,
                }),
            }
        }
        Some(mut existing) => {
            if resources::fill_missing_yolo_keys(ws, &mut existing)? {
                api.replace(&name, &PostParams::default(), &existing)
                    .await
                    .map_err(|source| OperatorError::Kube {
                        context: "backfill missing keys in YOLO Secret",
                        source,
                    })?;
            }
            Ok(())
        }
    }
}

async fn apply_managed_external_secret(
    client: &Client,
    ns: &str,
    ws: &WaddleServer,
    bindings: Option<&ResolvedRuntimeBindings>,
) -> Result<(), OperatorError> {
    let version = match bindings.and_then(|b| b.eso_api_version) {
        Some(v) => v,
        None => {
            let caps = external_crds::discover_capabilities(client).await;
            caps.eso_version.ok_or_else(|| {
                OperatorError::InvalidClusterState(
                    "External Secrets Operator CRD is not installed".to_owned(),
                )
            })?
        }
    };

    let desired = resources::build_managed_external_secret(ws, version)?;
    let es_name = resources::managed_external_secret_name(ws)?;
    let ar = external_crds::api_resource_eso(version);
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), ns, &ar);
    let params = PatchParams::apply(FIELD_MANAGER_WORKLOAD).force();
    api.patch(&es_name, &params, &Patch::Apply(&desired))
        .await
        .map_err(|source| OperatorError::Kube {
            context: "apply managed ExternalSecret",
            source,
        })?;
    Ok(())
}

async fn apply_workload(
    client: &Client,
    ns: &str,
    ws: &WaddleServer,
    validated: &waddle_operator_crd::ValidatedSpec,
    bindings: &ResolvedRuntimeBindings,
) -> Result<(), OperatorError> {
    let name = resources::workload_name(ws)?;

    let sa = resources::build_service_account(ws)?;
    ssa_apply(
        &Api::<ServiceAccount>::namespaced(client.clone(), ns),
        name,
        &sa,
    )
    .await?;

    let cm = resources::build_config_map(ws, bindings)?;
    let cm_name = resources::config_map_name(ws)?;
    ssa_apply(
        &Api::<ConfigMap>::namespaced(client.clone(), ns),
        &cm_name,
        &cm,
    )
    .await?;
    let cm_digest = resources::config_map_digest(&cm)?;

    let svc_api: Api<Service> = Api::namespaced(client.clone(), ns);
    let http_svc = resources::build_http_service(ws)?;
    ssa_apply(&svc_api, name, &http_svc).await?;

    if let Some(pvc) = resources::build_pvc(ws)? {
        let pvc_api: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), ns);
        let pvc_name = resources::pvc_name(ws)?;
        ensure_pvc_created(&pvc_api, &pvc_name, &pvc).await?;
    }

    let headless_name = resources::headless_service_name(ws)?;
    match resources::build_headless_service(ws)? {
        Some(svc) => ssa_apply(&svc_api, &headless_name, &svc).await?,
        None => delete_if_exists(&svc_api, &headless_name).await?,
    }

    let s2s_name = resources::s2s_service_name(ws)?;
    match resources::build_s2s_service(ws)? {
        Some(svc) => ssa_apply(&svc_api, &s2s_name, &svc).await?,
        None => delete_if_exists(&svc_api, &s2s_name).await?,
    }

    let pdb_api: Api<PodDisruptionBudget> = Api::namespaced(client.clone(), ns);
    match resources::build_pdb(ws)? {
        Some(pdb) => ssa_apply(&pdb_api, name, &pdb).await?,
        None => delete_if_exists(&pdb_api, name).await?,
    }

    let np_api: Api<NetworkPolicy> = Api::namespaced(client.clone(), ns);
    match resources::build_network_policy(ws)? {
        Some(np) => ssa_apply(&np_api, name, &np).await?,
        None => delete_if_exists(&np_api, name).await?,
    }

    let deployment = resources::build_deployment(ws, validated, bindings, &cm_digest)?;
    ssa_apply(
        &Api::<Deployment>::namespaced(client.clone(), ns),
        name,
        &deployment,
    )
    .await?;

    Ok(())
}

async fn ensure_pvc_created(
    api: &Api<PersistentVolumeClaim>,
    name: &str,
    pvc: &PersistentVolumeClaim,
) -> Result<(), OperatorError> {
    if api
        .get_opt(name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get PersistentVolumeClaim",
            source,
        })?
        .is_some()
    {
        return Ok(());
    }

    match api.create(&PostParams::default(), pvc).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(status)) if status.code == 409 => Ok(()),
        Err(source) => Err(OperatorError::Kube {
            context: "create PersistentVolumeClaim",
            source,
        }),
    }
}

async fn ssa_apply<K>(api: &Api<K>, name: &str, obj: &K) -> Result<(), OperatorError>
where
    K: Resource + Serialize + Clone + Debug + serde::de::DeserializeOwned,
    K::DynamicType: Default,
{
    let params = PatchParams::apply(FIELD_MANAGER_WORKLOAD).force();
    api.patch(name, &params, &Patch::Apply(obj))
        .await
        .map_err(|source| OperatorError::Kube {
            context: "server-side apply owned resource",
            source,
        })?;
    Ok(())
}

async fn delete_if_exists<K>(api: &Api<K>, name: &str) -> Result<(), OperatorError>
where
    K: Resource + Clone + Debug + serde::de::DeserializeOwned,
    K::DynamicType: Default,
{
    match api.delete(name, &DeleteParams::default()).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(status)) if status.code == 404 => Ok(()),
        Err(source) => Err(OperatorError::Kube {
            context: "delete disabled optional owned resource",
            source,
        }),
    }
}

fn build_status(
    ws: &WaddleServer,
    outcome: &TransitionOutcome,
    snapshot: &WorldSnapshot,
) -> Result<WaddleServerStatus, OperatorError> {
    let (replicas, ready_replicas, updated_replicas) = snapshot.world.workload.replica_counts();
    let conditions = outcome
        .conditions
        .iter()
        .map(convert_condition)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(WaddleServerStatus {
        phase: map_phase(outcome.to_phase),
        observed_generation: Some(snapshot.world.generation),
        replicas,
        ready_replicas,
        updated_replicas,
        ready_summary: format!("{ready_replicas}/{replicas}"),
        secrets_mode: Some(ws.spec.secrets.mode),
        deployment_uuid: snapshot.resolved_deployment_uuid.clone(),
        dependencies: DependenciesStatus {
            postgres: snapshot.world.postgres.to_status_summary(),
            spicedb: snapshot.world.spicedb.to_status_summary(),
            secrets: snapshot.world.secrets.to_status_summary(),
        },
        conditions,
    })
}

fn convert_condition(record: &ConditionRecord) -> Result<KubeCondition, OperatorError> {
    let ts =
        k8s_openapi::jiff::Timestamp::from_str(&record.last_transition_time).map_err(|err| {
            OperatorError::InvalidClusterState(format!(
                "invalid condition timestamp '{}': {err}",
                record.last_transition_time
            ))
        })?;
    Ok(KubeCondition {
        type_: record.kind.as_str().to_owned(),
        status: record.status.as_str().to_owned(),
        reason: record.reason.clone(),
        message: record.message.clone(),
        observed_generation: Some(record.observed_generation),
        last_transition_time: Time(ts),
    })
}

const fn map_phase(phase: Phase) -> WaddleServerPhase {
    match phase {
        Phase::Initializing => WaddleServerPhase::Initializing,
        Phase::Pending => WaddleServerPhase::Pending,
        Phase::Provisioning => WaddleServerPhase::Provisioning,
        Phase::Reconciling => WaddleServerPhase::Reconciling,
        Phase::Ready => WaddleServerPhase::Ready,
        Phase::Degraded => WaddleServerPhase::Degraded,
        Phase::Failed => WaddleServerPhase::Failed,
        Phase::Terminating => WaddleServerPhase::Terminating,
    }
}

fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    input[..end].to_owned()
}
