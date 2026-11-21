use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{
    ConfigMap, PersistentVolumeClaim, Secret, Service, ServiceAccount,
};
use k8s_openapi::api::discovery::v1::EndpointSlice;
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::api::policy::v1::PodDisruptionBudget;
use kube::api::{Api, ApiResource, DynamicObject, Resource};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::events::{Recorder, Reporter};
use kube::runtime::reflector::{ObjectRef, Store};
use kube::runtime::watcher;
use kube::Client;
use tokio::sync::{oneshot, watch};
use waddle_operator_crd::{
    PostgresSourceMode, SecretProviderMode, SpiceDbSourceMode, WaddleServer, WaddleServerPhase,
};
use waddle_operator_fsm::{Phase, RequeuePolicy};

use crate::cli::RunArgs;
use crate::error::OperatorError;
use crate::executor;
use crate::external_crds;
use crate::health::HealthState;
use crate::leader_election::{LeaderConfig, LeaderGuard};
use crate::metrics::OperatorMetrics;
use crate::observer::{self, OperatorPolicy};
use crate::resources;

const ERROR_REQUEUE_SECS: u64 = 15;
const SERVICEACCOUNT_NS_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/namespace";

/// Shared controller dependencies injected into every reconciliation call.
#[derive(Clone)]
pub struct ControllerContext {
    /// Kubernetes API client.
    pub client: Client,
    /// Operator security policy and discovered CRDs.
    pub policy: OperatorPolicy,
    /// Shared Prometheus metrics registry.
    pub metrics: Arc<OperatorMetrics>,
    /// Optional Kubernetes event recorder.
    pub recorder: Option<Recorder>,
}

/// Reconciles a single `WaddleServer` resource through observation, pure FSM evaluation,
/// imperative command execution, and diff-gated status persistence.
///
/// # Errors
/// Returns [`OperatorError`] if observation, workload execution, or status patching fails.
pub async fn reconcile_single(
    ws: Arc<WaddleServer>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    ctx.metrics.record_reconcile();
    let name = resources::workload_name(&ws)?;
    let ns = resources::workload_namespace(&ws)?;

    let from_phase = ws
        .status
        .as_ref()
        .map_or(Phase::Initializing, |s| map_from_crd_phase(s.phase));

    let snapshot = observer::observe_world(&ctx.client, &ws, &ctx.policy).await?;
    let outcome = waddle_operator_fsm::evaluate(from_phase, &snapshot.world);

    executor::apply_commands(
        &ctx.client,
        &ws,
        &snapshot,
        &outcome.commands,
        ctx.recorder.as_ref(),
    )
    .await?;

    let status_patched =
        executor::patch_status_if_changed(&ctx.client, &ws, &outcome, &snapshot).await?;

    if outcome.to_phase == Phase::Terminating {
        ctx.metrics.remove_resource(ns, name);
    } else {
        ctx.metrics
            .record_phase(ns, name, outcome.from_phase, outcome.to_phase);
    }

    if outcome.from_phase != outcome.to_phase || status_patched {
        tracing::info!(
            namespace = ns,
            name = name,
            from_phase = outcome.from_phase.as_str(),
            to_phase = outcome.to_phase.as_str(),
            status_patched,
            commands = outcome.commands.len(),
            "reconciled WaddleServer"
        );
    }

    Ok(match outcome.requeue {
        RequeuePolicy::None => Action::await_change(),
        RequeuePolicy::AfterSeconds(secs) => Action::requeue(Duration::from_secs(secs)),
    })
}

/// Runs the Kubernetes controller loop with optional leader election, capability discovery,
/// scoped watches, and graceful shutdown.
///
/// # Errors
/// Returns [`OperatorError::LeadershipLost`] if active lease renewal fails, or Kubernetes client
/// initialization errors.
pub async fn run_controller(
    client: Client,
    args: RunArgs,
    health: Arc<HealthState>,
    metrics: Arc<OperatorMetrics>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), OperatorError> {
    let mut leader_guard = if args.leader_election {
        let (lease_ns, holder_id) = resolve_lease_identity(&args);
        let cfg = LeaderConfig::new(lease_ns, args.lease_name.clone(), holder_id);
        let Some(guard) = LeaderGuard::acquire(&client, cfg, shutdown.clone()).await? else {
            return Ok(());
        };
        Some(guard)
    } else {
        None
    };

    let capabilities = external_crds::discover_capabilities(&client).await;
    tracing::info!(
        has_cnpg = capabilities.has_cnpg,
        has_spicedb = capabilities.has_spicedb,
        eso_version = ?capabilities.eso_version,
        yolo_enabled = args.enable_yolo_mode,
        namespace = args.namespace.as_deref().unwrap_or("<all>"),
        "discovered cluster API capabilities"
    );

    let ns = args.namespace.as_deref();
    let waddle_api = scoped_api::<WaddleServer>(&client, ns);
    let mut controller = Controller::new(waddle_api, watcher::Config::default());
    let store = controller.store();

    let managed_secret_selector = format!(
        "{}={}",
        resources::MANAGED_BY_LABEL,
        resources::MANAGED_BY_VALUE
    );

    controller = controller
        .owns(
            scoped_api::<Deployment>(&client, ns),
            watcher::Config::default(),
        )
        .owns(
            scoped_api::<ServiceAccount>(&client, ns),
            watcher::Config::default(),
        )
        .owns(
            scoped_api::<ConfigMap>(&client, ns),
            watcher::Config::default(),
        )
        .owns(
            scoped_api::<Service>(&client, ns),
            watcher::Config::default(),
        )
        .owns(
            scoped_api::<PersistentVolumeClaim>(&client, ns),
            watcher::Config::default(),
        )
        .owns(
            scoped_api::<PodDisruptionBudget>(&client, ns),
            watcher::Config::default(),
        )
        .owns(
            scoped_api::<NetworkPolicy>(&client, ns),
            watcher::Config::default(),
        )
        .owns(
            scoped_api::<Secret>(&client, ns),
            watcher::Config::default().labels(&managed_secret_selector),
        );

    let ep_store = store.clone();
    controller = controller.watches_with(
        scoped_api::<EndpointSlice>(&client, ns),
        (),
        watcher::Config::default(),
        move |slice| map_endpoint_slice(&slice, &ep_store),
    );

    if capabilities.has_cnpg {
        let ar = external_crds::api_resource_cnpg();
        let api = scoped_dynamic_api(&client, ns, &ar);
        let cnpg_store = store.clone();
        controller = controller.watches_with(api, ar, watcher::Config::default(), move |obj| {
            map_cnpg(&obj, &cnpg_store)
        });
    }

    if capabilities.has_spicedb {
        let ar = external_crds::api_resource_spicedb();
        let api = scoped_dynamic_api(&client, ns, &ar);
        let sp_store = store.clone();
        controller = controller.watches_with(api, ar, watcher::Config::default(), move |obj| {
            map_spicedb(&obj, &sp_store)
        });
    }

    if let Some(eso_ver) = capabilities.eso_version {
        let ar = external_crds::api_resource_eso(eso_ver);
        let api = scoped_dynamic_api(&client, ns, &ar);
        let es_store = store;
        controller = controller.watches_with(api, ar, watcher::Config::default(), move |obj| {
            map_external_secret(&obj, &es_store)
        });
    }

    let (_, holder_id) = resolve_lease_identity(&args);
    let recorder = Recorder::new(
        client.clone(),
        Reporter {
            controller: "waddle-operator".to_owned(),
            instance: Some(holder_id),
        },
    );

    let ctx = Arc::new(ControllerContext {
        client: client.clone(),
        policy: OperatorPolicy {
            enable_yolo_mode: args.enable_yolo_mode,
            allowed_cluster_secret_stores: args.allowed_cluster_secret_stores,
            capabilities,
        },
        metrics,
        recorder: Some(recorder),
    });

    let leadership_lost = Arc::new(AtomicBool::new(false));
    let (ctrl_stop_tx, ctrl_stop_rx) = oneshot::channel::<()>();
    let lost_flag = Arc::clone(&leadership_lost);
    let health_monitor = Arc::clone(&health);
    let mut lost_rx = leader_guard.as_mut().and_then(LeaderGuard::take_lost_rx);

    tokio::spawn(async move {
        if let Some(rx) = lost_rx.take() {
            tokio::select! {
                res = rx => {
                    if res.is_ok() {
                        lost_flag.store(true, Ordering::Release);
                    }
                    health_monitor.set_ready(false);
                    let _ = ctrl_stop_tx.send(());
                }
                _ = wait_for_shutdown(&mut shutdown) => {
                    health_monitor.set_ready(false);
                    let _ = ctrl_stop_tx.send(());
                }
            }
        } else {
            wait_for_shutdown(&mut shutdown).await;
            health_monitor.set_ready(false);
            let _ = ctrl_stop_tx.send(());
        }
    });

    health.set_ready(true);

    controller
        .graceful_shutdown_on(async move {
            let _ = ctrl_stop_rx.await;
        })
        .run(reconcile_single, error_policy, ctx)
        .for_each(|res| async {
            if let Err(err) = res {
                tracing::warn!(error = %err, "controller stream item failed");
            }
        })
        .await;

    health.set_ready(false);

    let lost = leadership_lost.load(Ordering::Acquire);
    if let Some(guard) = leader_guard {
        if !lost {
            guard.release().await;
        }
    }

    if lost {
        return Err(OperatorError::LeadershipLost);
    }

    Ok(())
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

fn error_policy(ws: Arc<WaddleServer>, err: &OperatorError, ctx: Arc<ControllerContext>) -> Action {
    ctx.metrics.record_error();
    tracing::warn!(
        namespace = ws.metadata.namespace.as_deref().unwrap_or("<none>"),
        name = ws.metadata.name.as_deref().unwrap_or("<none>"),
        error = %err,
        "reconcile failed; scheduling retry"
    );
    Action::requeue(Duration::from_secs(ERROR_REQUEUE_SECS))
}

fn scoped_api<K>(client: &Client, ns: Option<&str>) -> Api<K>
where
    K: Resource<Scope = kube::core::NamespaceResourceScope>,
    K::DynamicType: Default,
{
    match ns {
        Some(namespace) => Api::namespaced(client.clone(), namespace),
        None => Api::all(client.clone()),
    }
}

fn scoped_dynamic_api(client: &Client, ns: Option<&str>, ar: &ApiResource) -> Api<DynamicObject> {
    match ns {
        Some(namespace) => Api::namespaced_with(client.clone(), namespace, ar),
        None => Api::all_with(client.clone(), ar),
    }
}

fn map_endpoint_slice(
    slice: &EndpointSlice,
    store: &Store<WaddleServer>,
) -> Vec<ObjectRef<WaddleServer>> {
    let Some(ns) = slice.metadata.namespace.as_deref() else {
        return Vec::new();
    };
    let Some(svc_name) = slice
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get("kubernetes.io/service-name"))
    else {
        return Vec::new();
    };

    store
        .state()
        .into_iter()
        .filter(|ws| ws.metadata.namespace.as_deref() == Some(ns))
        .filter(|ws| ws_references_service(ws, svc_name))
        .map(|ws| ObjectRef::from_obj(ws.as_ref()))
        .collect()
}

fn ws_references_service(ws: &WaddleServer, svc_name: &str) -> bool {
    let pg_matches = ws.spec.postgres.mode == PostgresSourceMode::External
        && ws
            .spec
            .postgres
            .external
            .as_ref()
            .and_then(|e| e.service_name.as_deref())
            == Some(svc_name);

    let sp_matches = match ws.spec.spicedb.mode {
        SpiceDbSourceMode::SpiceDbOperator => {
            ws.spec.spicedb.operator.as_ref().is_some_and(|op| {
                op.service_name.as_deref().unwrap_or(&op.cluster_name) == svc_name
            })
        }
        SpiceDbSourceMode::External => {
            ws.spec
                .spicedb
                .external
                .as_ref()
                .and_then(|ext| ext.service_name.as_deref())
                == Some(svc_name)
        }
    };

    pg_matches || sp_matches
}

fn map_cnpg(cluster: &DynamicObject, store: &Store<WaddleServer>) -> Vec<ObjectRef<WaddleServer>> {
    let (Some(ns), Some(cluster_name)) = (
        cluster.metadata.namespace.as_deref(),
        cluster.metadata.name.as_deref(),
    ) else {
        return Vec::new();
    };

    store
        .state()
        .into_iter()
        .filter(|ws| ws.metadata.namespace.as_deref() == Some(ns))
        .filter(|ws| {
            ws.spec.postgres.mode == PostgresSourceMode::CloudNativePg
                && ws
                    .spec
                    .postgres
                    .cnpg
                    .as_ref()
                    .is_some_and(|c| c.cluster_name == cluster_name)
        })
        .map(|ws| ObjectRef::from_obj(ws.as_ref()))
        .collect()
}

fn map_spicedb(
    cluster: &DynamicObject,
    store: &Store<WaddleServer>,
) -> Vec<ObjectRef<WaddleServer>> {
    let (Some(ns), Some(cluster_name)) = (
        cluster.metadata.namespace.as_deref(),
        cluster.metadata.name.as_deref(),
    ) else {
        return Vec::new();
    };

    store
        .state()
        .into_iter()
        .filter(|ws| ws.metadata.namespace.as_deref() == Some(ns))
        .filter(|ws| {
            ws.spec.spicedb.mode == SpiceDbSourceMode::SpiceDbOperator
                && ws
                    .spec
                    .spicedb
                    .operator
                    .as_ref()
                    .is_some_and(|op| op.cluster_name == cluster_name)
        })
        .map(|ws| ObjectRef::from_obj(ws.as_ref()))
        .collect()
}

fn map_external_secret(
    es: &DynamicObject,
    store: &Store<WaddleServer>,
) -> Vec<ObjectRef<WaddleServer>> {
    let (Some(ns), Some(es_name)) = (
        es.metadata.namespace.as_deref(),
        es.metadata.name.as_deref(),
    ) else {
        return Vec::new();
    };

    store
        .state()
        .into_iter()
        .filter(|ws| ws.metadata.namespace.as_deref() == Some(ns))
        .filter(|ws| {
            if ws.spec.secrets.mode != SecretProviderMode::ExternalSecrets {
                return false;
            }
            let Some(eso) = &ws.spec.secrets.external_secrets else {
                return false;
            };
            if eso.managed.is_some() {
                resources::managed_external_secret_name(ws)
                    .is_ok_and(|managed_name| managed_name == es_name)
            } else {
                eso.external_secret_name.as_deref() == Some(es_name)
            }
        })
        .map(|ws| ObjectRef::from_obj(ws.as_ref()))
        .collect()
}

fn resolve_lease_identity(args: &RunArgs) -> (String, String) {
    let lease_ns = args
        .lease_namespace
        .clone()
        .or_else(|| args.namespace.clone())
        .or_else(|| {
            std::env::var("POD_NAMESPACE")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::fs::read_to_string(SERVICEACCOUNT_NS_PATH)
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "default".to_owned());

    let holder_id = args
        .holder_id
        .clone()
        .or_else(|| std::env::var("POD_NAME").ok().filter(|s| !s.is_empty()))
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| format!("waddle-operator-{}", uuid::Uuid::new_v4()));

    (lease_ns, holder_id)
}

const fn map_from_crd_phase(phase: WaddleServerPhase) -> Phase {
    match phase {
        WaddleServerPhase::Initializing => Phase::Initializing,
        WaddleServerPhase::Pending => Phase::Pending,
        WaddleServerPhase::Provisioning => Phase::Provisioning,
        WaddleServerPhase::Reconciling => Phase::Reconciling,
        WaddleServerPhase::Ready => Phase::Ready,
        WaddleServerPhase::Degraded => Phase::Degraded,
        WaddleServerPhase::Failed => Phase::Failed,
        WaddleServerPhase::Terminating => Phase::Terminating,
    }
}
