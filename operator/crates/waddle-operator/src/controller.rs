use std::hash::Hash;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::{Stream, StreamExt};
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
use kube::runtime::reflector::store::Writer;
use kube::runtime::reflector::{self, Lookup, ObjectRef, Store};
use kube::runtime::{watcher, WatchStreamExt};
use kube::Client;
use tokio::sync::watch;
use waddle_operator_crd::{
    AuthzBackendKind, PostgresSourceMode, SecretProviderMode, SpiceDbSourceMode,
    StorageBackendKind, WaddleServer, WaddleServerPhase, WaddleTenant,
};
use waddle_operator_fsm::{Phase, RequeuePolicy};

use crate::cli::RunArgs;
use crate::error::OperatorError;
use crate::executor;
use crate::external_crds;
use crate::health::HealthState;
use crate::leader_election::{LeaderConfig, LeaderGuard, LeadershipFence};
use crate::metrics::OperatorMetrics;
use crate::observer::{self, OperatorPolicy};
use crate::resources;
use crate::tenant_controller::{reconcile_tenant, TenantControllerContext};

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
    /// Monotonic lease fence checked prior to mutating cluster state.
    pub leadership: LeadershipFence,
}

/// Reconciles a single `WaddleServer` resource through observation, pure FSM evaluation,
/// imperative command execution, and diff-gated status persistence.
///
/// # Errors
/// Returns [`OperatorError`] if leadership is invalid or observation, workload execution, or
/// status patching fails.
pub async fn reconcile_single(
    ws: Arc<WaddleServer>,
    ctx: Arc<ControllerContext>,
) -> Result<Action, OperatorError> {
    ctx.leadership.ensure_valid()?;
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
        &ctx.leadership,
    )
    .await?;

    let status_patched =
        executor::patch_status_if_changed(&ctx.client, &ws, &outcome, &snapshot, &ctx.leadership)
            .await?;

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

/// Runs both the `WaddleServer` and `WaddleTenant` Kubernetes controller loops concurrently
/// with optional leader election, capability discovery, scoped watches, and graceful shutdown.
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

    let (stop_tx, stop_rx) = watch::channel(false);

    let ns = args.namespace.as_deref();
    let waddle_api = scoped_api::<WaddleServer>(&client, ns);
    let (store, writer) = reflector::store::<WaddleServer>();
    let waddle_stream = primary_watch_stream(
        writer,
        watcher::watcher(waddle_api, watcher::Config::default()),
        Arc::clone(&health),
        stop_rx.clone(),
    );
    let mut controller = Controller::for_stream(waddle_stream, store.clone());

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
        );

    if args.enable_yolo_mode {
        controller = controller.owns(
            scoped_api::<Secret>(&client, ns),
            watcher::Config::default().labels(&managed_secret_selector),
        );
    }

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

    let tenant_api = scoped_api::<WaddleTenant>(&client, ns);
    let mut tenant_controller = Controller::new(tenant_api, watcher::Config::default());
    let tenant_store = tenant_controller.store();
    tenant_controller = tenant_controller
        .owns(
            scoped_api::<ConfigMap>(&client, ns),
            watcher::Config::default(),
        )
        .watches_with(
            scoped_api::<WaddleServer>(&client, ns),
            (),
            watcher::Config::default(),
            move |ws| map_server_to_tenants(&ws, &tenant_store),
        );

    let (_, holder_id) = resolve_lease_identity(&args);
    let recorder = Recorder::new(
        client.clone(),
        Reporter {
            controller: "waddle-operator".to_owned(),
            instance: Some(holder_id),
        },
    );

    let leadership = leader_guard
        .as_ref()
        .map_or_else(LeadershipFence::always_active, LeaderGuard::fence);

    let ctx = Arc::new(ControllerContext {
        client: client.clone(),
        policy: OperatorPolicy {
            enable_yolo_mode: args.enable_yolo_mode,
            allowed_cluster_secret_stores: args.allowed_cluster_secret_stores,
            capabilities,
        },
        metrics: Arc::clone(&metrics),
        recorder: Some(recorder.clone()),
        leadership: leadership.clone(),
    });

    let tenant_ctx = Arc::new(TenantControllerContext {
        client: client.clone(),
        metrics,
        recorder: Some(recorder),
        leadership,
        openfga_endpoint_override: None,
    });

    let leadership_lost = Arc::new(AtomicBool::new(false));
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
                    let _ = stop_tx.send(true);
                    health_monitor.set_ready(false);
                }
                _ = wait_for_shutdown(&mut shutdown) => {
                    let _ = stop_tx.send(true);
                    health_monitor.set_ready(false);
                }
            }
        } else {
            wait_for_shutdown(&mut shutdown).await;
            let _ = stop_tx.send(true);
            health_monitor.set_ready(false);
        }
    });

    let mut server_stop_rx = stop_rx.clone();
    let server_fut = controller
        .graceful_shutdown_on(async move {
            wait_for_shutdown(&mut server_stop_rx).await;
        })
        .run(reconcile_single, error_policy, ctx)
        .for_each(|res| async {
            if let Err(err) = res {
                tracing::warn!(error = %err, "WaddleServer controller stream item failed");
            }
        });

    let mut tenant_stop_rx = stop_rx;
    let tenant_fut = tenant_controller
        .graceful_shutdown_on(async move {
            wait_for_shutdown(&mut tenant_stop_rx).await;
        })
        .run(reconcile_tenant, tenant_error_policy, tenant_ctx)
        .for_each(|res| async {
            if let Err(err) = res {
                tracing::warn!(error = %err, "WaddleTenant controller stream item failed");
            }
        });

    tokio::join!(server_fut, tenant_fut);

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

/// Drives the primary reflector stream and flips `/readyz` ready on the first
/// `watcher::Event::InitDone` without calling `Store::wait_until_ready()`.
///
/// `kube_runtime::reflector::Store::wait_until_ready` is backed by a single-waker
/// `oneshot::Receiver` inside `DelayedInit`, which `Controller::run`'s internal
/// `applier` already awaits. Any second concurrent waiter overwrites the waker and
/// starves either `/readyz` or reconciliation startup.
fn primary_watch_stream<K, S>(
    writer: Writer<K>,
    watch_stream: S,
    health: Arc<HealthState>,
    stop_rx: watch::Receiver<bool>,
) -> impl Stream<Item = watcher::Result<K>>
where
    K: Lookup + Clone + 'static,
    K::DynamicType: Eq + Hash + Clone,
    S: Stream<Item = watcher::Result<watcher::Event<K>>>,
{
    let synced_once = AtomicBool::new(false);
    reflector::reflector(writer, watch_stream)
        .inspect(move |event| {
            if matches!(event, Ok(watcher::Event::InitDone))
                && !synced_once.swap(true, Ordering::AcqRel)
                && !*stop_rx.borrow()
            {
                health.set_ready(true);
                if *stop_rx.borrow() {
                    health.set_ready(false);
                } else {
                    tracing::info!("primary WaddleServer reflector synced; marking operator ready");
                }
            }
        })
        .applied_objects()
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
    if matches!(err, OperatorError::LeadershipLost) {
        tracing::warn!(
            namespace = ws.metadata.namespace.as_deref().unwrap_or("<none>"),
            name = ws.metadata.name.as_deref().unwrap_or("<none>"),
            "leadership lost during WaddleServer reconciliation; halting retries"
        );
        return Action::await_change();
    }
    tracing::warn!(
        namespace = ws.metadata.namespace.as_deref().unwrap_or("<none>"),
        name = ws.metadata.name.as_deref().unwrap_or("<none>"),
        error = %err,
        "WaddleServer reconcile failed; scheduling retry"
    );
    Action::requeue(Duration::from_secs(ERROR_REQUEUE_SECS))
}

fn tenant_error_policy(
    wt: Arc<WaddleTenant>,
    err: &OperatorError,
    ctx: Arc<TenantControllerContext>,
) -> Action {
    ctx.metrics.record_error();
    if matches!(err, OperatorError::LeadershipLost) {
        tracing::warn!(
            namespace = wt.metadata.namespace.as_deref().unwrap_or("<none>"),
            name = wt.metadata.name.as_deref().unwrap_or("<none>"),
            "leadership lost during WaddleTenant reconciliation; halting retries"
        );
        return Action::await_change();
    }
    tracing::warn!(
        namespace = wt.metadata.namespace.as_deref().unwrap_or("<none>"),
        name = wt.metadata.name.as_deref().unwrap_or("<none>"),
        error = %err,
        "WaddleTenant reconcile failed; scheduling retry"
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

fn map_server_to_tenants(
    ws: &WaddleServer,
    store: &Store<WaddleTenant>,
) -> Vec<ObjectRef<WaddleTenant>> {
    let (Some(ns), Some(server_name)) = (
        ws.metadata.namespace.as_deref(),
        ws.metadata.name.as_deref(),
    ) else {
        return Vec::new();
    };

    store
        .state()
        .into_iter()
        .filter(|wt| wt.metadata.namespace.as_deref() == Some(ns))
        .filter(|wt| wt.spec.server_ref.name == server_name)
        .map(|wt| ObjectRef::from_obj(wt.as_ref()))
        .collect()
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
    let storage_matches = ws.spec.storage.backend == StorageBackendKind::Postgres
        && ws.spec.storage.postgres.as_ref().is_some_and(|pg| {
            pg.mode == PostgresSourceMode::External
                && pg.external.as_ref().and_then(|e| e.service_name.as_deref()) == Some(svc_name)
        });

    let authz_matches =
        match ws.spec.authz.backend {
            AuthzBackendKind::OpenFga => ws
                .spec
                .authz
                .openfga
                .as_ref()
                .is_some_and(|fga| fga.service_name == svc_name),
            AuthzBackendKind::SpiceDb => ws.spec.authz.spicedb.as_ref().is_some_and(|sp| match sp
                .mode
            {
                SpiceDbSourceMode::SpiceDbOperator => sp.operator.as_ref().is_some_and(|op| {
                    op.service_name.as_deref().unwrap_or(&op.cluster_name) == svc_name
                }),
                SpiceDbSourceMode::External => {
                    sp.external
                        .as_ref()
                        .and_then(|ext| ext.service_name.as_deref())
                        == Some(svc_name)
                }
            }),
        };

    storage_matches || authz_matches
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
            ws.spec.storage.backend == StorageBackendKind::Postgres
                && ws.spec.storage.postgres.as_ref().is_some_and(|pg| {
                    pg.mode == PostgresSourceMode::CloudNativePg
                        && pg
                            .cnpg
                            .as_ref()
                            .is_some_and(|c| c.cluster_name == cluster_name)
                })
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
            ws.spec.authz.backend == AuthzBackendKind::SpiceDb
                && ws.spec.authz.spicedb.as_ref().is_some_and(|sp| {
                    sp.mode == SpiceDbSourceMode::SpiceDbOperator
                        && sp
                            .operator
                            .as_ref()
                            .is_some_and(|op| op.cluster_name == cluster_name)
                })
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

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::sync::Arc;
    use std::time::Duration;

    use futures::channel::mpsc;
    use futures::StreamExt;
    use k8s_openapi::api::core::v1::ConfigMap;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use kube::runtime::reflector::{self, ObjectRef};
    use kube::runtime::watcher;
    use tokio::sync::watch;

    use super::primary_watch_stream;
    use crate::health::HealthState;
    use crate::metrics::OperatorMetrics;

    fn sample_config_map(name: &str) -> ConfigMap {
        ConfigMap {
            metadata: ObjectMeta {
                name: Some(name.to_owned()),
                namespace: Some("default".to_owned()),
                ..ObjectMeta::default()
            },
            ..ConfigMap::default()
        }
    }

    #[tokio::test]
    async fn primary_watch_stream_marks_ready_on_init_done_without_starving_store_waiter() {
        let (store, writer) = reflector::store::<ConfigMap>();
        let health = Arc::new(HealthState::new(Arc::new(OperatorMetrics::new())));
        let (_stop_tx, stop_rx) = watch::channel(false);

        let waiter_store = store.clone();
        let waiter = tokio::spawn(async move { waiter_store.wait_until_ready().await });
        tokio::task::yield_now().await;

        let (tx, rx) = mpsc::unbounded::<watcher::Result<watcher::Event<ConfigMap>>>();
        let mut stream = pin!(primary_watch_stream(
            writer,
            rx,
            Arc::clone(&health),
            stop_rx,
        ));

        let cm = sample_config_map("waddle-a");
        assert!(tx.unbounded_send(Ok(watcher::Event::Init)).is_ok());
        assert!(tx
            .unbounded_send(Ok(watcher::Event::InitApply(cm.clone())))
            .is_ok());

        let first = stream.next().await;
        assert!(
            matches!(first, Some(Ok(ref obj)) if obj.metadata.name.as_deref() == Some("waddle-a"))
        );
        assert!(!health.is_ready());

        assert!(tx.unbounded_send(Ok(watcher::Event::InitDone)).is_ok());
        drop(tx);

        assert!(stream.next().await.is_none());
        assert!(health.is_ready());
        assert!(store.get(&ObjectRef::from_obj(&cm)).is_some());

        let waiter_res = tokio::time::timeout(Duration::from_secs(2), waiter).await;
        assert!(matches!(waiter_res, Ok(Ok(Ok(())))));
    }

    #[tokio::test]
    async fn primary_watch_stream_marks_ready_on_empty_initial_list() {
        let (store, writer) = reflector::store::<ConfigMap>();
        let health = Arc::new(HealthState::new(Arc::new(OperatorMetrics::new())));
        let (_stop_tx, stop_rx) = watch::channel(false);

        let waiter_store = store.clone();
        let waiter = tokio::spawn(async move { waiter_store.wait_until_ready().await });
        tokio::task::yield_now().await;

        let (tx, rx) = mpsc::unbounded::<watcher::Result<watcher::Event<ConfigMap>>>();
        let stream = primary_watch_stream(writer, rx, Arc::clone(&health), stop_rx);

        assert!(tx.unbounded_send(Ok(watcher::Event::Init)).is_ok());
        assert!(tx.unbounded_send(Ok(watcher::Event::InitDone)).is_ok());
        drop(tx);

        let items: Vec<_> = stream.collect().await;
        assert!(items.is_empty());
        assert!(health.is_ready());

        let waiter_res = tokio::time::timeout(Duration::from_secs(2), waiter).await;
        assert!(matches!(waiter_res, Ok(Ok(Ok(())))));
    }

    #[tokio::test]
    async fn primary_watch_stream_does_not_mark_ready_when_stopping() {
        let (store, writer) = reflector::store::<ConfigMap>();
        let health = Arc::new(HealthState::new(Arc::new(OperatorMetrics::new())));
        let (stop_tx, stop_rx) = watch::channel(false);

        assert!(stop_tx.send(true).is_ok());

        let (tx, rx) = mpsc::unbounded::<watcher::Result<watcher::Event<ConfigMap>>>();
        let stream = primary_watch_stream(writer, rx, Arc::clone(&health), stop_rx);

        assert!(tx.unbounded_send(Ok(watcher::Event::Init)).is_ok());
        assert!(tx.unbounded_send(Ok(watcher::Event::InitDone)).is_ok());
        drop(tx);

        let items: Vec<_> = stream.collect().await;
        assert!(items.is_empty());
        assert!(!health.is_ready());
        assert!(store.wait_until_ready().await.is_ok());
    }

    #[tokio::test]
    async fn primary_watch_stream_does_not_re_ready_on_relist_after_shutdown() {
        let (_store, writer) = reflector::store::<ConfigMap>();
        let health = Arc::new(HealthState::new(Arc::new(OperatorMetrics::new())));
        let (stop_tx, stop_rx) = watch::channel(false);

        let (tx, rx) = mpsc::unbounded::<watcher::Result<watcher::Event<ConfigMap>>>();
        let mut stream = pin!(primary_watch_stream(
            writer,
            rx,
            Arc::clone(&health),
            stop_rx,
        ));

        let cm = sample_config_map("waddle-b");
        assert!(tx.unbounded_send(Ok(watcher::Event::Init)).is_ok());
        assert!(tx.unbounded_send(Ok(watcher::Event::InitDone)).is_ok());
        assert!(tx
            .unbounded_send(Ok(watcher::Event::Apply(cm.clone())))
            .is_ok());

        let applied = stream.next().await;
        assert!(
            matches!(applied, Some(Ok(ref obj)) if obj.metadata.name.as_deref() == Some("waddle-b"))
        );
        assert!(health.is_ready());

        assert!(stop_tx.send(true).is_ok());
        health.set_ready(false);

        assert!(tx.unbounded_send(Ok(watcher::Event::Init)).is_ok());
        assert!(tx.unbounded_send(Ok(watcher::Event::InitDone)).is_ok());
        drop(tx);

        assert!(stream.next().await.is_none());
        assert!(!health.is_ready());
    }
}
