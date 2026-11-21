//! End-to-end reconciler integration tests against an in-process mock Kubernetes API server.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{Method, StatusCode};
use axum::response::Response;
use axum::Router;
use base64::Engine;
use http::Request;
use kube::runtime::events::{Recorder, Reporter};
use kube::{Client, Config};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tower::ServiceExt;
use waddle_operator::external_crds::{discover_capabilities, DiscoveredCapabilities};
use waddle_operator::health::{probe_router, HealthState};
use waddle_operator::leader_election::{LeaderConfig, LeaderGuard, LeadershipFence};
use waddle_operator::metrics::OperatorMetrics;
use waddle_operator::resources::ANNOTATION_SESSION_KEY_FINGERPRINT;
use waddle_operator::{reconcile_single, ControllerContext, OperatorError, OperatorPolicy};
use waddle_operator_crd::{WaddleServer, WaddleServerPhase};

#[derive(Default)]
struct MockClusterInner {
    cnpg_installed: bool,
    spicedb_installed: bool,
    eso_installed: bool,
    stall_lease_puts: bool,
    resources: BTreeMap<String, Value>,
    gets: Vec<String>,
    mutations: Vec<(String, String)>,
    events: Vec<Value>,
    rv_counter: u64,
    conflict_secret_once: Option<(String, Value)>,
}

#[derive(Clone)]
struct MockCluster {
    inner: Arc<Mutex<MockClusterInner>>,
    base_uri: http::Uri,
}

impl MockCluster {
    async fn start() -> Self {
        let inner = Arc::new(Mutex::new(MockClusterInner {
            cnpg_installed: true,
            spicedb_installed: true,
            eso_installed: true,
            rv_counter: 1,
            ..MockClusterInner::default()
        }));

        let app = Router::new()
            .fallback(handle_k8s_request)
            .with_state(Arc::clone(&inner));
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind loopback port");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let base_uri = format!("http://{addr}")
            .parse::<http::Uri>()
            .expect("valid loopback uri");
        Self { inner, base_uri }
    }

    fn client(&self) -> Client {
        Client::try_from(Config::new(self.base_uri.clone())).expect("mock kube client")
    }

    fn set_crds_installed(&self, cnpg: bool, spicedb: bool, eso: bool) {
        let mut g = self.inner.lock().expect("lock");
        g.cnpg_installed = cnpg;
        g.spicedb_installed = spicedb;
        g.eso_installed = eso;
    }

    fn put(&self, path: &str, value: Value) {
        self.inner
            .lock()
            .expect("lock")
            .resources
            .insert(path.to_owned(), value);
    }

    fn get(&self, path: &str) -> Option<Value> {
        self.inner
            .lock()
            .expect("lock")
            .resources
            .get(path)
            .cloned()
    }

    fn mutations(&self) -> Vec<(String, String)> {
        self.inner.lock().expect("lock").mutations.clone()
    }

    fn clear_mutations(&self) {
        self.inner.lock().expect("lock").mutations.clear();
    }

    fn gets(&self) -> Vec<String> {
        self.inner.lock().expect("lock").gets.clone()
    }

    fn clear_gets(&self) {
        self.inner.lock().expect("lock").gets.clear();
    }

    fn set_stall_lease_puts(&self, stall: bool) {
        self.inner.lock().expect("lock").stall_lease_puts = stall;
    }

    fn delete(&self, path: &str) {
        self.inner.lock().expect("lock").resources.remove(path);
    }

    fn events(&self) -> Vec<Value> {
        self.inner.lock().expect("lock").events.clone()
    }

    fn inject_secret_409_conflict(&self, secret_path: &str, existing_secret: Value) {
        let mut g = self.inner.lock().expect("lock");
        g.conflict_secret_once = Some((secret_path.to_owned(), existing_secret));
    }

    fn get_waddle(&self, ns: &str, name: &str) -> WaddleServer {
        let path = format!("/apis/waddle.social/v1alpha1/namespaces/{ns}/waddleservers/{name}");
        let val = self.get(&path).expect("WaddleServer must exist");
        serde_json::from_value(val).expect("valid WaddleServer JSON")
    }
}

async fn handle_k8s_request(
    State(state): State<Arc<Mutex<MockClusterInner>>>,
    method: Method,
    uri: OriginalUri,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let query = uri.query().unwrap_or("");
    let should_stall_lease = {
        let g = state.lock().expect("lock");
        g.stall_lease_puts && method == Method::PUT && path.contains("/leases/")
    };
    if should_stall_lease {
        tokio::time::sleep(Duration::from_secs(4)).await;
    }

    let mut g = state.lock().expect("lock");

    // Discovery routes
    if method == Method::GET && path == "/apis/postgresql.cnpg.io/v1" {
        return if g.cnpg_installed {
            json_response(
                StatusCode::OK,
                json!({
                    "kind": "APIResourceList",
                    "apiVersion": "v1",
                    "groupVersion": "postgresql.cnpg.io/v1",
                    "resources": [{ "name": "clusters", "singularName": "cluster", "namespaced": true, "kind": "Cluster", "verbs": ["get", "list", "watch"] }]
                }),
            )
        } else {
            missing_crd_response()
        };
    }
    if method == Method::GET && path == "/apis/authzed.com/v1alpha1" {
        return if g.spicedb_installed {
            json_response(
                StatusCode::OK,
                json!({
                    "kind": "APIResourceList",
                    "apiVersion": "v1",
                    "groupVersion": "authzed.com/v1alpha1",
                    "resources": [{ "name": "spicedbclusters", "singularName": "spicedbcluster", "namespaced": true, "kind": "SpiceDBCluster", "verbs": ["get", "list", "watch"] }]
                }),
            )
        } else {
            missing_crd_response()
        };
    }
    if method == Method::GET && path == "/apis/external-secrets.io/v1" {
        return if g.eso_installed {
            json_response(
                StatusCode::OK,
                json!({
                    "kind": "APIResourceList",
                    "apiVersion": "v1",
                    "groupVersion": "external-secrets.io/v1",
                    "resources": [{ "name": "externalsecrets", "singularName": "externalsecret", "namespaced": true, "kind": "ExternalSecret", "verbs": ["get", "list", "watch", "patch", "create"] }]
                }),
            )
        } else {
            missing_crd_response()
        };
    }
    if method == Method::GET && path == "/apis/external-secrets.io/v1beta1" {
        return missing_crd_response();
    }

    // EndpointSlice list
    if method == Method::GET && path.ends_with("/endpointslices") {
        let expected_svc = query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            if k == "labelSelector" {
                v.strip_prefix("kubernetes.io/service-name%3D")
                    .or_else(|| v.strip_prefix("kubernetes.io/service-name="))
            } else {
                None
            }
        });
        let prefix = format!("{path}/");
        let items: Vec<Value> = g
            .resources
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v.clone())
            .filter(|item| match expected_svc {
                Some(svc) => {
                    item["metadata"]["labels"]["kubernetes.io/service-name"].as_str() == Some(svc)
                }
                None => true,
            })
            .collect();
        return json_response(
            StatusCode::OK,
            json!({
                "apiVersion": "discovery.k8s.io/v1",
                "kind": "EndpointSliceList",
                "metadata": { "resourceVersion": "1" },
                "items": items
            }),
        );
    }

    if method == Method::GET {
        g.gets.push(path.clone());
        if path.starts_with("/apis/postgresql.cnpg.io/v1/") && !g.cnpg_installed {
            return missing_crd_response();
        }
        if path.starts_with("/apis/authzed.com/v1alpha1/") && !g.spicedb_installed {
            return missing_crd_response();
        }
        if path.starts_with("/apis/external-secrets.io/v1/") && !g.eso_installed {
            return missing_crd_response();
        }
        if let Some(obj) = g.resources.get(&path) {
            return json_response(StatusCode::OK, obj.clone());
        }
        return not_found_response(&path);
    }

    g.mutations.push((method.as_str().to_owned(), path.clone()));

    let payload: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };

    if method == Method::POST {
        if path.ends_with("/events") {
            g.events.push(payload.clone());
            return json_response(StatusCode::CREATED, payload);
        }
        let name = payload["metadata"]["name"]
            .as_str()
            .unwrap_or("unnamed")
            .to_owned();
        let item_path = format!("{path}/{name}");

        if let Some((conflict_path, concurrent_obj)) = g.conflict_secret_once.take() {
            if item_path == conflict_path {
                g.resources.insert(item_path, concurrent_obj);
                return json_response(
                    StatusCode::CONFLICT,
                    json!({
                        "kind": "Status",
                        "apiVersion": "v1",
                        "status": "Failure",
                        "reason": "AlreadyExists",
                        "message": "secrets already exists",
                        "code": 409
                    }),
                );
            }
            g.conflict_secret_once = Some((conflict_path, concurrent_obj));
        }

        if g.resources.contains_key(&item_path) {
            return json_response(
                StatusCode::CONFLICT,
                json!({
                    "kind": "Status",
                    "apiVersion": "v1",
                    "status": "Failure",
                    "reason": "AlreadyExists",
                    "code": 409
                }),
            );
        }

        let mut stored = payload;
        g.rv_counter += 1;
        let rv = g.rv_counter.to_string();
        if let Some(meta) = stored.get_mut("metadata").and_then(Value::as_object_mut) {
            meta.insert("resourceVersion".to_owned(), Value::String(rv));
        }
        g.resources.insert(item_path, stored.clone());
        return json_response(StatusCode::CREATED, stored);
    }

    if method == Method::PUT {
        let mut stored = payload;
        g.rv_counter += 1;
        let rv = g.rv_counter.to_string();
        if let Some(meta) = stored.get_mut("metadata").and_then(Value::as_object_mut) {
            meta.insert("resourceVersion".to_owned(), Value::String(rv));
        }
        g.resources.insert(path, stored.clone());
        return json_response(StatusCode::OK, stored);
    }

    if method == Method::PATCH {
        if let Some(parent_path) = path.strip_suffix("/status") {
            let Some(mut parent) = g.resources.get(parent_path).cloned() else {
                return not_found_response(parent_path);
            };
            if let Some(obj) = parent.as_object_mut() {
                obj.insert("status".to_owned(), payload["status"].clone());
            }
            g.rv_counter += 1;
            let rv = g.rv_counter.to_string();
            if let Some(meta) = parent.get_mut("metadata").and_then(Value::as_object_mut) {
                meta.insert("resourceVersion".to_owned(), Value::String(rv));
            }
            g.resources.insert(parent_path.to_owned(), parent.clone());
            return json_response(StatusCode::OK, parent);
        }

        let url_name = path.rsplit('/').next().unwrap_or("");
        if let Some(body_name) = payload["metadata"]["name"].as_str() {
            if body_name != url_name {
                return json_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    json!({
                        "kind": "Status",
                        "apiVersion": "v1",
                        "status": "Failure",
                        "reason": "Invalid",
                        "message": format!("the name of the object ({body_name}) must match the name on the URL ({url_name})"),
                        "code": 422
                    }),
                );
            }
        }

        let existing = g.resources.get(&path).cloned();
        let mut stored = payload;
        if path.contains("/configmaps/") {
            if let Some(prev) = &existing {
                for field in ["data", "binaryData"] {
                    if let Some(prev_entries) = prev.get(field).and_then(Value::as_object) {
                        let stored_obj = stored.as_object_mut().and_then(|root| {
                            root.entry(field.to_owned())
                                .or_insert_with(|| json!({}))
                                .as_object_mut()
                        });
                        if let Some(dst) = stored_obj {
                            for (k, v) in prev_entries {
                                dst.entry(k.clone()).or_insert_with(|| v.clone());
                            }
                        }
                    }
                }
            }
        }
        if path.contains("/externalsecrets/") {
            if let Some(target) = stored
                .get_mut("spec")
                .and_then(|s| s.get_mut("target"))
                .and_then(Value::as_object_mut)
            {
                target
                    .entry("deletionPolicy".to_owned())
                    .or_insert_with(|| json!("Retain"));
            }
            if let Some(entries) = stored
                .get_mut("spec")
                .and_then(|s| s.get_mut("data"))
                .and_then(Value::as_array_mut)
            {
                for item in entries {
                    if let Some(rr) = item.get_mut("remoteRef").and_then(Value::as_object_mut) {
                        rr.entry("conversionStrategy".to_owned())
                            .or_insert_with(|| json!("Default"));
                        rr.entry("decodingStrategy".to_owned())
                            .or_insert_with(|| json!("None"));
                        rr.entry("metadataPolicy".to_owned())
                            .or_insert_with(|| json!("None"));
                    }
                }
            }
        }
        let prev_gen = existing
            .as_ref()
            .and_then(|e| e["metadata"]["generation"].as_i64())
            .unwrap_or(0);
        let spec_changed = existing
            .as_ref()
            .is_none_or(|e| e.get("spec") != stored.get("spec"));
        let next_gen = if spec_changed {
            prev_gen + 1
        } else {
            prev_gen.max(1)
        };

        if let Some(prev_status) = existing.as_ref().and_then(|e| e.get("status")).cloned() {
            if stored.get("status").is_none() {
                if let Some(obj) = stored.as_object_mut() {
                    obj.insert("status".to_owned(), prev_status);
                }
            }
        }

        g.rv_counter += 1;
        let rv = g.rv_counter.to_string();
        if let Some(meta) = stored.get_mut("metadata").and_then(Value::as_object_mut) {
            meta.insert("resourceVersion".to_owned(), Value::String(rv));
            meta.insert("generation".to_owned(), json!(next_gen));
        }
        g.resources.insert(path, stored.clone());
        return json_response(StatusCode::OK, stored);
    }

    if method == Method::DELETE {
        if g.resources.remove(&path).is_none() {
            return not_found_response(&path);
        }
        return json_response(
            StatusCode::OK,
            json!({ "kind": "Status", "apiVersion": "v1", "status": "Success" }),
        );
    }

    json_response(StatusCode::METHOD_NOT_ALLOWED, json!({}))
}

fn json_response(status: StatusCode, body: Value) -> Response {
    let bytes = serde_json::to_vec(&body).expect("serialize mock body");
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(bytes))
        .expect("build response")
}

fn missing_crd_response() -> Response {
    json_response(
        StatusCode::NOT_FOUND,
        json!({
            "kind": "Status",
            "apiVersion": "v1",
            "status": "Failure",
            "reason": "NotFound",
            "message": "the server could not find the requested resource",
            "code": 404
        }),
    )
}

fn not_found_response(path: &str) -> Response {
    json_response(
        StatusCode::NOT_FOUND,
        json!({
            "kind": "Status",
            "apiVersion": "v1",
            "status": "Failure",
            "reason": "NotFound",
            "message": format!("{path} not found"),
            "code": 404
        }),
    )
}

fn make_ctx(
    cluster: &MockCluster,
    yolo: bool,
    caps: DiscoveredCapabilities,
) -> Arc<ControllerContext> {
    let client = cluster.client();
    let recorder = Recorder::new(
        client.clone(),
        Reporter {
            controller: "waddle-operator".to_owned(),
            instance: Some("test-pod".to_owned()),
        },
    );
    Arc::new(ControllerContext {
        client,
        policy: OperatorPolicy {
            enable_yolo_mode: yolo,
            allowed_cluster_secret_stores: vec!["vault-backend".to_owned()],
            capabilities: caps,
        },
        metrics: Arc::new(OperatorMetrics::new()),
        recorder: Some(recorder),
        leadership: LeadershipFence::always_active(),
    })
}

fn yolo_waddle_json(ns: &str, name: &str) -> Value {
    json!({
        "apiVersion": "waddle.social/v1alpha1",
        "kind": "WaddleServer",
        "metadata": {
            "name": name,
            "namespace": ns,
            "uid": "aaaaaaaa-bbbb-4ccc-8ddd-111111111111",
            "generation": 1
        },
        "spec": {
            "baseUrl": "https://waddle.local",
            "xmpp": { "domain": "waddle.local" },
            "postgres": { "mode": "CloudNativePg", "cnpg": { "clusterName": "pg" } },
            "spicedb": { "mode": "SpiceDbOperator", "operator": { "clusterName": "spicedb" } },
            "secrets": { "mode": "Yolo" }
        }
    })
}

fn seed_healthy_cnpg_and_spicedb(cluster: &MockCluster, ns: &str) {
    cluster.put(
        &format!("/apis/postgresql.cnpg.io/v1/namespaces/{ns}/clusters/pg"),
        json!({
            "apiVersion": "postgresql.cnpg.io/v1",
            "kind": "Cluster",
            "metadata": { "name": "pg", "namespace": ns },
            "spec": { "bootstrap": { "initdb": { "database": "waddle" } } },
            "status": {
                "readyInstances": 1,
                "currentPrimary": "pg-1",
                "conditions": [{ "type": "Ready", "status": "True", "reason": "ClusterIsReady" }]
            }
        }),
    );
    cluster.put(
        &format!("/api/v1/namespaces/{ns}/secrets/pg-app"),
        json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": "pg-app", "namespace": ns },
            "data": { "uri": base64::engine::general_purpose::STANDARD.encode("postgres://u:p@pg-rw:5432/waddle") }
        }),
    );
    cluster.put(
        &format!("/apis/authzed.com/v1alpha1/namespaces/{ns}/spicedbclusters/spicedb"),
        json!({
            "apiVersion": "authzed.com/v1alpha1",
            "kind": "SpiceDBCluster",
            "metadata": { "name": "spicedb", "namespace": ns, "generation": 1 },
            "spec": { "secretName": "spicedb-key", "config": { "datastoreEngine": "postgres" } },
            "status": {
                "observedGeneration": 1,
                "currentMigrationHash": "m1",
                "targetMigrationHash": "m1",
                "conditions": []
            }
        }),
    );
    cluster.put(
        &format!("/api/v1/namespaces/{ns}/secrets/spicedb-key"),
        json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": "spicedb-key", "namespace": ns },
            "data": { "preshared_key": base64::engine::general_purpose::STANDARD.encode("spicedb-secret-value") }
        }),
    );
    set_spicedb_endpoints(cluster, ns, "spicedb", true);
}

fn set_spicedb_endpoints(cluster: &MockCluster, ns: &str, svc: &str, ready: bool) {
    cluster.put(
        &format!("/apis/discovery.k8s.io/v1/namespaces/{ns}/endpointslices/{svc}-slice"),
        json!({
            "apiVersion": "discovery.k8s.io/v1",
            "kind": "EndpointSlice",
            "metadata": {
                "name": format!("{svc}-slice"),
                "namespace": ns,
                "labels": { "kubernetes.io/service-name": svc }
            },
            "addressType": "IPv4",
            "endpoints": [{ "addresses": ["10.0.0.10"], "conditions": { "ready": ready } }]
        }),
    );
}

fn mark_deployment_available(cluster: &MockCluster, ns: &str, name: &str) {
    let path = format!("/apis/apps/v1/namespaces/{ns}/deployments/{name}");
    let mut dep = cluster.get(&path).expect("deployment created");
    let gen = dep["metadata"]["generation"].as_i64().unwrap_or(1);
    dep["status"] = json!({
        "observedGeneration": gen,
        "replicas": 1,
        "updatedReplicas": 1,
        "readyReplicas": 1,
        "availableReplicas": 1,
        "conditions": [
            { "type": "Available", "status": "True", "reason": "MinimumReplicasAvailable" },
            { "type": "Progressing", "status": "True", "reason": "NewReplicaSetAvailable" }
        ]
    });
    cluster.put(&path, dep);
}

#[tokio::test]
async fn pending_when_crds_missing_and_full_yolo_lifecycle_to_ready_with_idempotency() {
    let cluster = MockCluster::start().await;
    cluster.set_crds_installed(false, false, false);
    let ns = "dev";
    let name = "waddle-yolo";
    cluster.put(
        &format!("/apis/waddle.social/v1alpha1/namespaces/{ns}/waddleservers/{name}"),
        yolo_waddle_json(ns, name),
    );

    let caps = discover_capabilities(&cluster.client()).await;
    assert!(!caps.has_cnpg);
    assert!(!caps.has_spicedb);
    let ctx = make_ctx(&cluster, true, caps);

    // Pass 1: Missing external CRDs -> stays Pending, creates zero workload resources.
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("reconcile with missing CRDs succeeds");
    let ws1 = cluster.get_waddle(ns, name);
    let st1 = ws1.status.as_ref().expect("status set");
    assert_eq!(st1.phase, WaddleServerPhase::Pending);
    assert_eq!(st1.dependencies.postgres.reason, "CrdNotInstalled");
    assert_eq!(st1.dependencies.spicedb.reason, "CrdNotInstalled");
    assert!(
        cluster
            .get(&format!("/apis/apps/v1/namespaces/{ns}/deployments/{name}"))
            .is_none(),
        "must not create Deployment while Pending"
    );

    // Install CRDs + healthy Postgres + SpiceDB
    cluster.set_crds_installed(true, true, true);
    seed_healthy_cnpg_and_spicedb(&cluster, ns);
    let ctx = make_ctx(
        &cluster,
        true,
        discover_capabilities(&cluster.client()).await,
    );

    // Pass 2: Converges through Provisioning (creates YOLO secret) and stays in Provisioning
    // on this snapshot until next reconcile reads back the newly created secret, OR wait:
    // pure FSM checks snapshot before commands -> Pending -> Provisioning (EnsureYoloSecret).
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("reconcile provisioning");
    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Provisioning
    );
    assert!(cluster
        .get(&format!(
            "/api/v1/namespaces/{ns}/secrets/{name}-yolo-secrets"
        ))
        .is_some());

    // Pass 3: Observes ready YOLO secret -> transitions Provisioning -> Reconciling & creates workload.
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("reconcile workload");
    let ws3 = cluster.get_waddle(ns, name);
    let st3 = ws3.status.as_ref().expect("status");
    assert_eq!(st3.phase, WaddleServerPhase::Reconciling);
    assert!(st3.deployment_uuid.is_some());
    assert!(cluster
        .get(&format!("/apis/apps/v1/namespaces/{ns}/deployments/{name}"))
        .is_some());

    // Pass 4: Deployment reports Available -> transitions Reconciling -> Ready.
    mark_deployment_available(&cluster, ns, name);
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("reconcile ready");
    let ws4 = cluster.get_waddle(ns, name);
    let st4 = ws4.status.as_ref().expect("status");
    assert_eq!(st4.phase, WaddleServerPhase::Ready);
    assert_eq!(st4.ready_summary, "1/1");

    // Pass 5: Idempotency — zero mutations (0 status PATCH, 0 workload apply)!
    cluster.clear_mutations();
    reconcile_single(Arc::new(ws4), Arc::clone(&ctx))
        .await
        .expect("idempotent reconcile");
    assert!(
        cluster.mutations().is_empty(),
        "converged Ready WaddleServer MUST emit zero status PATCHes or resource writes, got: {:?}",
        cluster.mutations()
    );
}

#[tokio::test]
async fn yolo_409_conflict_preserves_existing_keys_and_never_overwrites() {
    let cluster = MockCluster::start().await;
    let ns = "race";
    let name = "app";
    cluster.put(
        &format!("/apis/waddle.social/v1alpha1/namespaces/{ns}/waddleservers/{name}"),
        yolo_waddle_json(ns, name),
    );
    seed_healthy_cnpg_and_spicedb(&cluster, ns);

    let preset_session = "A".repeat(48);
    let preset_occupant = "B".repeat(48);
    let preset_uuid = "99999999-8888-4777-8666-555555555555";
    let b64 = base64::engine::general_purpose::STANDARD;
    cluster.inject_secret_409_conflict(
        &format!("/api/v1/namespaces/{ns}/secrets/{name}-yolo-secrets"),
        json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": format!("{name}-yolo-secrets"), "namespace": ns, "resourceVersion": "42" },
            "type": "Opaque",
            "data": {
                "WADDLE_SESSION_KEY": b64.encode(&preset_session),
                "WADDLE_OCCUPANT_ID_SECRET": b64.encode(&preset_occupant),
                "WADDLE_DEPLOYMENT_UUID": b64.encode(preset_uuid),
            }
        }),
    );

    let ctx = make_ctx(
        &cluster,
        true,
        discover_capabilities(&cluster.client()).await,
    );
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("409 conflict handled cleanly");

    let stored = cluster
        .get(&format!(
            "/api/v1/namespaces/{ns}/secrets/{name}-yolo-secrets"
        ))
        .expect("secret exists after 409");
    let data = stored["data"].as_object().expect("secret data");
    assert_eq!(
        data["WADDLE_SESSION_KEY"].as_str(),
        Some(b64.encode(&preset_session).as_str())
    );
    assert_eq!(
        data["WADDLE_OCCUPANT_ID_SECRET"].as_str(),
        Some(b64.encode(&preset_occupant).as_str())
    );
    assert_eq!(
        data["WADDLE_DEPLOYMENT_UUID"].as_str(),
        Some(b64.encode(preset_uuid).as_str())
    );
}

#[tokio::test]
async fn degraded_never_regresses_on_spicedb_outage_or_invalid_edit_and_eso_rotation_emits_event() {
    let cluster = MockCluster::start().await;
    let ns = "prod";
    let name = "waddle";
    let es_name = "waddle-runtime-secrets";
    let b64 = base64::engine::general_purpose::STANDARD;

    cluster.put(
        &format!("/apis/waddle.social/v1alpha1/namespaces/{ns}/waddleservers/{name}"),
        json!({
            "apiVersion": "waddle.social/v1alpha1",
            "kind": "WaddleServer",
            "metadata": {
                "name": name,
                "namespace": ns,
                "uid": "bbbbbbbb-cccc-4ddd-8eee-222222222222",
                "generation": 1
            },
            "spec": {
                "baseUrl": "https://chat.waddle.social",
                "xmpp": { "domain": "chat.waddle.social" },
                "postgres": { "mode": "CloudNativePg", "cnpg": { "clusterName": "pg" } },
                "spicedb": { "mode": "SpiceDbOperator", "operator": { "clusterName": "spicedb" } },
                "deployment": { "uuid": "cccccccc-dddd-4eee-8fff-333333333333" },
                "secrets": {
                    "mode": "ExternalSecrets",
                    "externalSecrets": {
                        "apiVersion": "V1",
                        "managed": {
                            "secretStoreRef": { "kind": "ClusterSecretStore", "name": "vault-backend" },
                            "remoteRefs": {
                                "sessionKey": { "key": "kv/waddle", "property": "session" },
                                "occupantIdSecret": { "key": "kv/waddle", "property": "occupant" },
                                "spicedbPresharedKey": { "key": "kv/waddle", "property": "spicedb" }
                            }
                        }
                    }
                }
            }
        }),
    );
    seed_healthy_cnpg_and_spicedb(&cluster, ns);

    let ctx = make_ctx(
        &cluster,
        false,
        discover_capabilities(&cluster.client()).await,
    );

    // Step 1: Creates managed ExternalSecret -> Provisioning
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("managed ES applied");
    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Provisioning
    );

    // Mark ExternalSecret Ready=True + populate target Secret
    let mut es = cluster
        .get(&format!(
            "/apis/external-secrets.io/v1/namespaces/{ns}/externalsecrets/{es_name}"
        ))
        .expect("managed ExternalSecret created");
    es["status"] = json!({
        "conditions": [{ "type": "Ready", "status": "True", "reason": "SecretSynced" }]
    });
    cluster.put(
        &format!("/apis/external-secrets.io/v1/namespaces/{ns}/externalsecrets/{es_name}"),
        es,
    );
    cluster.put(
        &format!("/api/v1/namespaces/{ns}/secrets/{es_name}"),
        json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": es_name, "namespace": ns },
            "data": {
                "WADDLE_SESSION_KEY": b64.encode("session-secret-key-at-least-32-bytes-long"),
                "WADDLE_OCCUPANT_ID_SECRET": b64.encode("occupant-secret-key-at-least-32-bytes-long"),
                "WADDLE_SPICEDB_PRESHARED_KEY": b64.encode("spicedb-preshared-key-value"),
            }
        }),
    );

    // Step 2: Reconciling -> Ready
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("apply workload");
    mark_deployment_available(&cluster, ns, name);
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("reach Ready");
    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Ready
    );

    // Step 3: SpiceDB loses all ready endpoints -> Ready -> Degraded (never Pending!)
    set_spicedb_endpoints(&cluster, ns, "spicedb", false);
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("degrade on spicedb outage");
    let degraded_ws = cluster.get_waddle(ns, name);
    assert_eq!(
        degraded_ws.status.as_ref().expect("st").phase,
        WaddleServerPhase::Degraded
    );
    assert!(cluster
        .get(&format!("/apis/apps/v1/namespaces/{ns}/deployments/{name}"))
        .is_some());

    // Recover SpiceDB -> Degraded -> Ready
    set_spicedb_endpoints(&cluster, ns, "spicedb", true);
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("recover to Ready");
    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Ready
    );

    // Step 4: Rotate WADDLE_SESSION_KEY in ESO Secret -> updates pod fingerprint + emits Warning event
    let prev_fp = cluster
        .get(&format!("/apis/apps/v1/namespaces/{ns}/deployments/{name}"))
        .expect("dep")["spec"]["template"]["metadata"]["annotations"]
        [ANNOTATION_SESSION_KEY_FINGERPRINT]
        .as_str()
        .expect("fp present")
        .to_owned();
    cluster.put(
        &format!("/api/v1/namespaces/{ns}/secrets/{es_name}"),
        json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": es_name, "namespace": ns },
            "data": {
                "WADDLE_SESSION_KEY": b64.encode("ROTATED-session-secret-key-at-least-32-bytes"),
                "WADDLE_OCCUPANT_ID_SECRET": b64.encode("occupant-secret-key-at-least-32-bytes-long"),
                "WADDLE_SPICEDB_PRESHARED_KEY": b64.encode("spicedb-preshared-key-value"),
            }
        }),
    );
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("rotation detected");
    let new_fp = cluster
        .get(&format!("/apis/apps/v1/namespaces/{ns}/deployments/{name}"))
        .expect("dep")["spec"]["template"]["metadata"]["annotations"]
        [ANNOTATION_SESSION_KEY_FINGERPRINT]
        .as_str()
        .expect("new fp")
        .to_owned();
    assert_ne!(
        prev_fp, new_fp,
        "fingerprint annotation must update on rotation"
    );
    assert!(
        cluster
            .events()
            .iter()
            .any(|ev| ev["reason"].as_str() == Some("SensitiveSecretRotated")),
        "expected SensitiveSecretRotated warning event, events={:?}",
        cluster.events()
    );

    // Step 5: Post-Ready invalid spec edit -> Degraded (never Failed, Deployment preserved)
    mark_deployment_available(&cluster, ns, name);
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("re-converge Ready");
    let ws_path = format!("/apis/waddle.social/v1alpha1/namespaces/{ns}/waddleservers/{name}");
    let mut bad_ws = cluster.get(&ws_path).expect("ws");
    bad_ws["spec"]["baseUrl"] = json!("not-a-valid-url");
    bad_ws["metadata"]["generation"] = json!(2);
    cluster.put(&ws_path, bad_ws);
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("invalid post-Ready edit keeps running Deployment");
    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Degraded
    );
    assert!(cluster
        .get(&format!("/apis/apps/v1/namespaces/{ns}/deployments/{name}"))
        .is_some());
}

#[tokio::test]
async fn health_and_metrics_endpoints_report_readiness_and_prometheus_counters() {
    let metrics = Arc::new(OperatorMetrics::new());
    metrics.record_reconcile();
    metrics.record_phase(
        "default",
        "demo",
        waddle_operator_fsm::Phase::Initializing,
        waddle_operator_fsm::Phase::Ready,
    );
    let state = Arc::new(HealthState::new(metrics));
    let router = probe_router(Arc::clone(&state));

    let res_unready = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(axum::body::Body::empty())
                .expect("req"),
        )
        .await
        .expect("readyz");
    assert_eq!(res_unready.status(), StatusCode::SERVICE_UNAVAILABLE);

    state.set_ready(true);
    let res_ready = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(axum::body::Body::empty())
                .expect("req"),
        )
        .await
        .expect("readyz");
    assert_eq!(res_ready.status(), StatusCode::OK);

    let res_metrics = router
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(axum::body::Body::empty())
                .expect("req"),
        )
        .await
        .expect("metrics");
    let body = axum::body::to_bytes(res_metrics.into_body(), 1024 * 64)
        .await
        .expect("metrics body");
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("waddle_operator_reconciliations_total 1"));
    assert!(text.contains("waddle_operator_resources_by_phase{phase=\"Ready\"} 1"));
}

#[tokio::test]
async fn clustered_s2s_eso_lineage_and_defaults_tolerant_reconciliation() {
    let cluster = MockCluster::start().await;
    let ns = "cluster-ns";
    let name = "waddle";
    let es_name = "waddle-runtime-secrets";
    let b64 = base64::engine::general_purpose::STANDARD;

    let mut key_entries = Vec::new();
    for i in 0..4u8 {
        let seed = [i + 1; 32];
        key_entries.push(b64.encode(seed));
    }
    let keypool_csv = key_entries.join(",");

    let ws_path = format!("/apis/waddle.social/v1alpha1/namespaces/{ns}/waddleservers/{name}");
    cluster.put(
        &ws_path,
        json!({
            "apiVersion": "waddle.social/v1alpha1",
            "kind": "WaddleServer",
            "metadata": {
                "name": name,
                "namespace": ns,
                "uid": "cccccccc-1111-4222-8333-444444444444",
                "generation": 1
            },
            "spec": {
                "replicas": 2,
                "baseUrl": "https://chat.example.com",
                "xmpp": { "domain": "chat.example.com" },
                "postgres": { "mode": "CloudNativePg", "cnpg": { "clusterName": "pg" } },
                "spicedb": { "mode": "SpiceDbOperator", "operator": { "clusterName": "spicedb" } },
                "deployment": {
                    "uuid": "dddddddd-1111-4222-8333-444444444444",
                    "lineageAction": "enroll"
                },
                "s3": {
                    "endpoint": "https://s3.example.com",
                    "bucket": "waddle-media"
                },
                "s2s": {
                    "enabled": true,
                    "port": 5269,
                    "configMapName": "waddle-s2s"
                },
                "clustering": {
                    "enabled": true,
                    "swarmPort": 7900,
                    "enrolledKeyCount": 4,
                    "keypairHeadroom": 2
                },
                "persistence": {
                    "enabled": true,
                    "accessModes": ["ReadWriteMany"],
                    "size": "20Gi"
                },
                "networkPolicy": { "enabled": true },
                "secrets": {
                    "mode": "ExternalSecrets",
                    "externalSecrets": {
                        "apiVersion": "V1",
                        "managed": {
                            "secretStoreRef": { "kind": "ClusterSecretStore", "name": "vault-backend" },
                            "remoteRefs": {
                                "sessionKey": { "key": "kv/waddle", "property": "session" },
                                "occupantIdSecret": { "key": "kv/waddle", "property": "occupant" },
                                "spicedbPresharedKey": { "key": "kv/waddle", "property": "spicedb" },
                                "s3AccessKeyId": { "key": "kv/waddle", "property": "s3_id" },
                                "s3SecretAccessKey": { "key": "kv/waddle", "property": "s3_secret" },
                                "clusteringKeypairPool": { "key": "kv/waddle", "property": "pool" }
                            }
                        }
                    }
                }
            }
        }),
    );
    seed_healthy_cnpg_and_spicedb(&cluster, ns);

    let ctx = make_ctx(
        &cluster,
        false,
        discover_capabilities(&cluster.client()).await,
    );

    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("managed ExternalSecret created with ESO defaults");
    let st1 = cluster.get_waddle(ns, name).status.expect("st");
    assert_eq!(
        st1.phase,
        WaddleServerPhase::Provisioning,
        "expected Provisioning, got status={st1:?}"
    );

    let es_path = format!("/apis/external-secrets.io/v1/namespaces/{ns}/externalsecrets/{es_name}");
    let mut es = cluster
        .get(&es_path)
        .expect("managed ExternalSecret present");
    assert_eq!(
        es["spec"]["target"]["deletionPolicy"].as_str(),
        Some("Retain"),
        "mock should simulate ESO CRD defaults on stored ExternalSecret"
    );
    es["status"] = json!({
        "conditions": [{ "type": "Ready", "status": "True", "reason": "SecretSynced" }]
    });
    cluster.put(&es_path, es);
    cluster.put(
        &format!("/api/v1/namespaces/{ns}/secrets/{es_name}"),
        json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": es_name, "namespace": ns },
            "data": {
                "WADDLE_SESSION_KEY": b64.encode("session-secret-key-at-least-32-bytes-long"),
                "WADDLE_OCCUPANT_ID_SECRET": b64.encode("occupant-secret-key-at-least-32-bytes-long"),
                "WADDLE_SPICEDB_PRESHARED_KEY": b64.encode("spicedb-psk"),
                "WADDLE_S3_ACCESS_KEY_ID": b64.encode("minioadmin"),
                "WADDLE_S3_SECRET_ACCESS_KEY": b64.encode("miniosecret"),
                "WADDLE_CLUSTERING_KEYPAIR_POOL": b64.encode(keypool_csv),
            }
        }),
    );

    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("ESO CRD defaults must not trap Provisioning; workload must apply");
    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Reconciling
    );

    let np_path =
        format!("/apis/networking.k8s.io/v1/namespaces/{ns}/networkpolicies/{name}-swarm");
    let np = cluster
        .get(&np_path)
        .expect("<name>-swarm NetworkPolicy applied");
    assert_eq!(np["metadata"]["name"].as_str(), Some("waddle-swarm"));

    let cm = cluster
        .get(&format!("/api/v1/namespaces/{ns}/configmaps/{name}-config"))
        .expect("ConfigMap created");
    assert_eq!(
        cm["data"]["WADDLE_DB_LINEAGE_ACTION"].as_str(),
        Some("enroll"),
        "spec.deployment.lineageAction must propagate in ExternalSecrets mode"
    );

    let mut dep = cluster
        .get(&format!("/apis/apps/v1/namespaces/{ns}/deployments/{name}"))
        .expect("deployment created");
    let gen = dep["metadata"]["generation"].as_i64().unwrap_or(1);
    dep["status"] = json!({
        "observedGeneration": gen,
        "replicas": 2,
        "updatedReplicas": 2,
        "readyReplicas": 2,
        "availableReplicas": 2,
        "conditions": [
            { "type": "Available", "status": "True", "reason": "MinimumReplicasAvailable" },
            { "type": "Progressing", "status": "True", "reason": "NewReplicaSetAvailable" }
        ]
    });
    cluster.put(
        &format!("/apis/apps/v1/namespaces/{ns}/deployments/{name}"),
        dep,
    );
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("converge Ready");
    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Ready
    );

    // Disable NetworkPolicy -> must delete `<name>-swarm` cleanly (not `<name>`).
    let mut updated_ws = cluster.get(&ws_path).expect("ws");
    updated_ws["spec"]["networkPolicy"]["enabled"] = json!(false);
    updated_ws["metadata"]["generation"] = json!(2);
    cluster.put(&ws_path, updated_ws);
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("disable netpol deletes <name>-swarm");
    assert!(
        cluster.get(&np_path).is_none(),
        "<name>-swarm NetworkPolicy must be deleted when networkPolicy.enabled=false"
    );
}

#[tokio::test]
async fn invalid_spec_skips_dependency_gets_and_stale_deadline_stays_reconciling() {
    let cluster = MockCluster::start().await;
    let ns = "sec";
    let name = "bad";
    let ws_path = format!("/apis/waddle.social/v1alpha1/namespaces/{ns}/waddleservers/{name}");

    cluster.put(
        &ws_path,
        json!({
            "apiVersion": "waddle.social/v1alpha1",
            "kind": "WaddleServer",
            "metadata": {
                "name": name,
                "namespace": ns,
                "uid": "eeeeeeee-1111-4222-8333-555555555555",
                "generation": 1
            },
            "spec": {
                "baseUrl": "https://sec.local",
                "xmpp": { "domain": "sec.local" },
                "postgres": {
                    "mode": "External",
                    "external": { "secretName": "../../other-ns/secrets/stolen" }
                },
                "spicedb": {
                    "mode": "External",
                    "external": { "endpoint": "http://spicedb:50051", "insecure": true }
                },
                "secrets": { "mode": "Yolo" }
            }
        }),
    );

    let ctx = make_ctx(
        &cluster,
        true,
        discover_capabilities(&cluster.client()).await,
    );
    cluster.clear_gets();
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("invalid spec transitions cleanly to Failed without Kube path error");

    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Failed
    );
    let gets = cluster.gets();
    assert!(
        gets.iter()
            .all(|p| !p.contains("/secrets/") && !p.contains("other-ns")),
        "invalid spec MUST NOT query Secrets or traversed namespaces, got gets={gets:?}"
    );

    // Part 2: Stale gen-1 ProgressDeadlineExceeded while gen-2 is waiting on observedGeneration=1.
    let name_stale = "rollout";
    let ws_stale_path =
        format!("/apis/waddle.social/v1alpha1/namespaces/{ns}/waddleservers/{name_stale}");
    let mut ws_stale = yolo_waddle_json(ns, name_stale);
    ws_stale["status"] = json!({
        "phase": "Reconciling",
        "replicas": 1,
        "readyReplicas": 0,
        "updatedReplicas": 0,
        "readySummary": "0/1",
        "dependencies": {
            "postgres": { "phase": "Ready", "reason": "Ready" },
            "spicedb": { "phase": "Ready", "reason": "Ready" },
            "secrets": { "phase": "Ready", "reason": "Ready" }
        }
    });
    cluster.put(&ws_stale_path, ws_stale);
    seed_healthy_cnpg_and_spicedb(&cluster, ns);

    reconcile_single(
        Arc::new(cluster.get_waddle(ns, name_stale)),
        Arc::clone(&ctx),
    )
    .await
    .expect("create YOLO secret");
    reconcile_single(
        Arc::new(cluster.get_waddle(ns, name_stale)),
        Arc::clone(&ctx),
    )
    .await
    .expect("create workload");

    let dep_path = format!("/apis/apps/v1/namespaces/{ns}/deployments/{name_stale}");
    let mut dep = cluster.get(&dep_path).expect("dep exists");
    dep["metadata"]["generation"] = json!(2);
    dep["status"] = json!({
        "observedGeneration": 1,
        "replicas": 1,
        "updatedReplicas": 1,
        "readyReplicas": 0,
        "availableReplicas": 0,
        "conditions": [
            {
                "type": "Progressing",
                "status": "False",
                "reason": "ProgressDeadlineExceeded",
                "message": "ReplicaSet timed out on gen 1"
            }
        ]
    });
    cluster.put(&dep_path, dep);

    reconcile_single(
        Arc::new(cluster.get_waddle(ns, name_stale)),
        Arc::clone(&ctx),
    )
    .await
    .expect("stale ProgressDeadlineExceeded handled cleanly");
    assert_eq!(
        cluster.get_waddle(ns, name_stale).status.expect("st").phase,
        WaddleServerPhase::Reconciling,
        "stale gen-1 ProgressDeadlineExceeded while observedGeneration=1 < generation=2 must stay Reconciling"
    );
}

#[tokio::test]
async fn spicedb_generation_drift_service_pg_secret_rotation_ownership_and_lease_deadline() {
    let cluster = MockCluster::start().await;
    let ns = "hardening";
    let name = "chat";
    let b64 = base64::engine::general_purpose::STANDARD;

    // 1. Stale SpiceDBCluster metadata.generation (2) > observedGeneration (1) blocks Pending.
    cluster.put(
        &format!("/apis/waddle.social/v1alpha1/namespaces/{ns}/waddleservers/{name}"),
        yolo_waddle_json(ns, name),
    );
    seed_healthy_cnpg_and_spicedb(&cluster, ns);
    let sp_path = format!("/apis/authzed.com/v1alpha1/namespaces/{ns}/spicedbclusters/spicedb");
    let mut sp = cluster.get(&sp_path).expect("spicedb exists");
    sp["metadata"]["generation"] = json!(2);
    sp["status"]["observedGeneration"] = json!(1);
    cluster.put(&sp_path, sp);

    let ctx = make_ctx(
        &cluster,
        true,
        discover_capabilities(&cluster.client()).await,
    );
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("reconcile with stale SpiceDBCluster generation");
    let st_stale = cluster.get_waddle(ns, name).status.expect("st");
    assert_eq!(st_stale.phase, WaddleServerPhase::Pending);
    assert_eq!(st_stale.dependencies.spicedb.reason, "GenerationPending");

    // Advance SpiceDBCluster observedGeneration to 2 -> reach Ready.
    let mut sp2 = cluster.get(&sp_path).expect("spicedb exists");
    sp2["status"]["observedGeneration"] = json!(2);
    cluster.put(&sp_path, sp2);

    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("provision yolo secret");
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("create workload");
    mark_deployment_available(&cluster, ns, name);
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("converge Ready");
    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Ready
    );

    // 2. Pre-existing Service/chat-headless owned by ANOTHER WaddleServer MUST NOT be deleted.
    let unrelated_svc_path = format!("/api/v1/namespaces/{ns}/services/{name}-headless");
    cluster.put(
        &unrelated_svc_path,
        json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {
                "name": format!("{name}-headless"),
                "namespace": ns,
                "uid": "99999999-aaaa-4bbb-8ccc-dddddddddddd",
                "ownerReferences": [{
                    "apiVersion": "waddle.social/v1alpha1",
                    "kind": "WaddleServer",
                    "name": "chat-headless",
                    "uid": "other-owner-uid",
                    "controller": true
                }]
            }
        }),
    );

    // 3. Extra ConfigMap keys from other field managers must NOT trigger drift repairs,
    // while deleted HTTP Service, tampered operator ConfigMap keys, and out-of-band
    // Deployment field edits MUST self-heal.
    let http_svc_path = format!("/api/v1/namespaces/{ns}/services/{name}");
    let cm_path = format!("/api/v1/namespaces/{ns}/configmaps/{name}-config");
    let mut cm_extra = cluster.get(&cm_path).expect("cm");
    let orig_http_addr = cm_extra["data"]["WADDLE_HTTP_ADDR"]
        .as_str()
        .expect("WADDLE_HTTP_ADDR in ConfigMap")
        .to_owned();
    cm_extra["data"]["ADMIN_EXTRA_KEY"] = json!("kubectl-managed");
    cluster.put(&cm_path, cm_extra);

    cluster.clear_mutations();
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("extra ConfigMap key should stay Ready without rewrite");
    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Ready,
        "extra non-operator ConfigMap keys must not trigger Reconciling loops"
    );
    assert!(
        cluster.mutations().is_empty(),
        "extra non-operator ConfigMap keys must cause zero mutations, got: {:?}",
        cluster.mutations()
    );

    cluster.delete(&http_svc_path);
    let mut cm_tampered = cluster.get(&cm_path).expect("cm");
    cm_tampered["data"]["WADDLE_HTTP_ADDR"] = json!("127.0.0.1:9999");
    cluster.put(&cm_path, cm_tampered);

    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("detect Service deletion and ConfigMap tamper -> Reconciling");
    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Reconciling
    );
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("self-heal workload and restore Service & ConfigMap");
    assert!(
        cluster.get(&http_svc_path).is_some(),
        "deleted HTTP Service must be recreated on repair"
    );
    let repaired_cm = cluster.get(&cm_path).expect("repaired cm");
    assert_eq!(
        repaired_cm["data"]["WADDLE_HTTP_ADDR"].as_str(),
        Some(orig_http_addr.as_str()),
        "tampered operator ConfigMap key must be reconciled back to desired value"
    );
    assert_eq!(
        repaired_cm["data"]["ADMIN_EXTRA_KEY"].as_str(),
        Some("kubectl-managed"),
        "SSA repair must preserve extra non-operator ConfigMap keys"
    );
    assert!(
        cluster.get(&unrelated_svc_path).is_some(),
        "non-owned Service/chat-headless MUST NOT be deleted"
    );

    mark_deployment_available(&cluster, ns, name);
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("return Ready");

    // 3b. Out-of-band Deployment field edits (e.g. terminationGracePeriodSeconds) must self-heal.
    let dep_path = format!("/apis/apps/v1/namespaces/{ns}/deployments/{name}");
    let mut tampered_dep = cluster.get(&dep_path).expect("dep");
    let expected_grace = tampered_dep["spec"]["template"]["spec"]["terminationGracePeriodSeconds"]
        .as_i64()
        .expect("expected terminationGracePeriodSeconds");
    tampered_dep["spec"]["template"]["spec"]["terminationGracePeriodSeconds"] = json!(999);
    cluster.put(&dep_path, tampered_dep);

    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("detect out-of-band Deployment edit -> Reconciling");
    assert_eq!(
        cluster.get_waddle(ns, name).status.expect("st").phase,
        WaddleServerPhase::Reconciling
    );
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("repair Deployment terminationGracePeriodSeconds");
    let repaired_dep = cluster.get(&dep_path).expect("repaired dep");
    assert_eq!(
        repaired_dep["spec"]["template"]["spec"]["terminationGracePeriodSeconds"].as_i64(),
        Some(expected_grace),
        "tampered Deployment terminationGracePeriodSeconds must be reconciled back to desired value"
    );

    mark_deployment_available(&cluster, ns, name);
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("return Ready after Deployment repair");

    // 4. Rotate Postgres Secret pg-app -> pod template secrets-checksum MUST change.
    let prev_checksum = cluster.get(&dep_path).expect("dep")["spec"]["template"]["metadata"]
        ["annotations"]["waddle.social/secrets-checksum"]
        .as_str()
        .expect("prev checksum")
        .to_owned();
    cluster.put(
        &format!("/api/v1/namespaces/{ns}/secrets/pg-app"),
        json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": { "name": "pg-app", "namespace": ns },
            "data": { "uri": b64.encode("postgres://u:NEW_PASS@pg-rw:5432/waddle") }
        }),
    );
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("detect PG secret rotation -> Reconciling");
    reconcile_single(Arc::new(cluster.get_waddle(ns, name)), Arc::clone(&ctx))
        .await
        .expect("apply new secrets-checksum");
    let new_checksum = cluster.get(&dep_path).expect("dep")["spec"]["template"]["metadata"]
        ["annotations"]["waddle.social/secrets-checksum"]
        .as_str()
        .expect("new checksum")
        .to_owned();
    assert_ne!(
        prev_checksum, new_checksum,
        "Postgres secret rotation MUST update waddle.social/secrets-checksum on Deployment"
    );

    // 5. Stalled Lease renewal server must trigger LeaderGuard leadership loss before TTL expiry.
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut cfg = LeaderConfig::new(ns.to_owned(), "op-leader".to_owned(), "pod-a".to_owned());
    cfg.lease_duration_secs = 2;
    cfg.renew_interval = Duration::from_millis(100);
    cfg.retry_interval = Duration::from_millis(50);

    let mut guard = LeaderGuard::acquire(&cluster.client(), cfg, shutdown_rx)
        .await
        .expect("acquire lease")
        .expect("lease guard returned");
    let fenced_handle = guard.fence();
    assert!(
        fenced_handle.is_valid(),
        "LeadershipFence must be valid immediately after lease acquisition"
    );
    let lost_rx = guard.take_lost_rx().expect("lost rx");
    cluster.set_stall_lease_puts(true);

    tokio::time::timeout(Duration::from_secs(3), lost_rx)
        .await
        .expect(
            "LeaderGuard must report lost leadership within deadline bound before 4s stall returns",
        )
        .expect("lost notification delivered");

    assert!(
        !fenced_handle.is_valid(),
        "LeadershipFence must be revoked when LeaderGuard loses leadership"
    );

    let fenced_ctx = Arc::new(ControllerContext {
        client: ctx.client.clone(),
        policy: ctx.policy.clone(),
        metrics: Arc::clone(&ctx.metrics),
        recorder: ctx.recorder.clone(),
        leadership: fenced_handle,
    });
    cluster.clear_mutations();
    let lost_err = reconcile_single(Arc::new(cluster.get_waddle(ns, name)), fenced_ctx)
        .await
        .expect_err("reconcile_single with revoked LeadershipFence must abort");
    assert!(
        matches!(lost_err, OperatorError::LeadershipLost),
        "expected LeadershipLost error, got: {lost_err:?}"
    );
    assert!(
        cluster.mutations().is_empty(),
        "revoked leader fence must perform zero Kubernetes writes, got: {:?}",
        cluster.mutations()
    );
}
