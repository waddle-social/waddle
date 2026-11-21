//! Integration tests for Kubernetes resource builders and hash stability.

use std::collections::BTreeMap;

use k8s_openapi::ByteString;
use serde_json::json;
use waddle_operator::bindings::{PostgresBinding, ResolvedRuntimeBindings, SpiceDbBinding};
use waddle_operator::external_crds::DiscoveredEsoVersion;
use waddle_operator::resources::{
    build_config_map, build_deployment, build_headless_service, build_http_service,
    build_managed_external_secret, build_network_policy, build_pdb, build_pvc, build_s2s_service,
    build_service_account, build_yolo_secret, compute_workload_hash, config_map_digest,
    fill_missing_yolo_keys, managed_external_secret_drifted, ANNOTATION_SPEC_HASH,
    KEY_DEPLOYMENT_UUID, KEY_OCCUPANT_ID_SECRET, KEY_SESSION_KEY,
};
use waddle_operator::yolo::{
    generate_base64_secret_48, generate_ed25519_seed_base64, validate_keypair_pool, RedactedSecret,
};
use waddle_operator_crd::{validate_spec, ContentDigest, WaddleServer};

fn sample_eso_clustered_server() -> WaddleServer {
    serde_json::from_value(json!({
        "apiVersion": "waddle.social/v1alpha1",
        "kind": "WaddleServer",
        "metadata": {
            "name": "waddle-prod",
            "namespace": "waddle",
            "uid": "11111111-2222-4333-8444-555555555555"
        },
        "spec": {
            "baseUrl": "https://waddle.example",
            "xmpp": {
                "domain": "waddle.example",
                "publicWebsocketUrl": "wss://waddle.example/xmpp-websocket",
                "tlsSecretName": "waddle-xmpp-tls"
            },
            "postgres": {
                "mode": "CloudNativePg",
                "cnpg": { "clusterName": "waddle-pg" },
                "restrictedRuntime": {
                    "enabled": true,
                    "releaseGitSha": "0123456789abcdef0123456789abcdef01234567",
                    "schema": "waddle",
                    "databaseHost": "waddle-pg-rw",
                    "databasePort": 5432,
                    "databaseName": "waddle",
                    "caSecretName": "waddle-pg-ca",
                    "caSecretKey": "ca.crt",
                    "runtimeCredentialSecret": "waddle-pg-runtime-creds",
                    "runtimeCredentialKey": "password",
                    "handoffConfigMapName": "waddle-pg-handoff",
                    "roles": {
                        "migratorLogin": "waddle_migrator",
                        "schemaOwner": "waddle_owner",
                        "runtimeLogin": "waddle_runtime",
                        "attestorInstallerLogin": "waddle_attestor_installer",
                        "attestorDefinerRole": "waddle_attestor_definer",
                        "attestorSchema": "waddle_attestor"
                    }
                }
            },
            "spicedb": {
                "mode": "SpiceDbOperator",
                "operator": { "clusterName": "spicedb" }
            },
            "secrets": {
                "mode": "ExternalSecrets",
                "externalSecrets": {
                    "apiVersion": "Auto",
                    "clusteringKeypoolSecretName": "waddle-keypool",
                    "extraSecretRefs": ["waddle-extra-env"],
                    "managed": {
                        "secretStoreRef": { "kind": "ClusterSecretStore", "name": "vault-backend" },
                        "refreshInterval": "30m",
                        "remoteRefs": {
                            "sessionKey": { "key": "waddle/prod", "property": "session_key" },
                            "occupantIdSecret": { "key": "waddle/prod", "property": "occupant_id" },
                            "spicedbPresharedKey": { "key": "waddle/prod", "property": "spicedb_psk" }
                        }
                    },
                    "apnsSecretRef": {
                        "secretName": "waddle-apns",
                        "secretKey": "AuthKey.p8",
                        "teamId": "TEAM123456",
                        "keyId": "KEY1234567",
                        "bundleId": "social.waddle.app"
                    }
                }
            },
            "deployment": {
                "uuid": "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
            },
            "s3": {
                "endpoint": "https://s3.example",
                "bucket": "waddle-uploads"
            },
            "s2s": {
                "enabled": true,
                "configMapName": "waddle-s2s-config"
            },
            "clustering": {
                "enabled": true,
                "enrolledKeyCount": 4,
                "keypairHeadroom": 1
            },
            "persistence": {
                "enabled": true,
                "accessModes": ["ReadWriteMany"],
                "size": "20Gi"
            }
        }
    }))
    .expect("valid WaddleServer JSON")
}

#[test]
fn deployment_and_companion_manifests_match_waddle_server_contracts() {
    let ws = sample_eso_clustered_server();
    let validated = validate_spec(&ws.spec).expect("spec must validate");
    let rr = ws
        .spec
        .postgres
        .restricted_runtime
        .clone()
        .expect("restricted runtime present");

    let bindings = ResolvedRuntimeBindings {
        postgres: PostgresBinding::RestrictedRuntime { spec: Box::new(rr) },
        spicedb: SpiceDbBinding {
            endpoint: "http://spicedb:50051".to_owned(),
            insecure: true,
            preshared_key_secret_name: "waddle-prod-runtime-secrets".to_owned(),
            preshared_key_secret_key: "WADDLE_SPICEDB_PRESHARED_KEY".to_owned(),
        },
        runtime_secret_name: "waddle-prod-runtime-secrets".to_owned(),
        extra_secret_refs: vec!["waddle-extra-env".to_owned()],
        clustering_keypool_secret_name: Some("waddle-keypool".to_owned()),
        deployment_uuid: ws.spec.deployment.uuid.clone(),
        lineage_action: None,
        secrets_checksum: Some(
            ContentDigest::try_new(&"a".repeat(64)).expect("64 hex chars valid"),
        ),
        session_key_fingerprint: Some(
            ContentDigest::try_new(&"b".repeat(64)).expect("64 hex chars valid"),
        ),
        occupant_id_fingerprint: Some(
            ContentDigest::try_new(&"c".repeat(64)).expect("64 hex chars valid"),
        ),
        eso_api_version: Some(DiscoveredEsoVersion::V1),
    };

    let sa = build_service_account(&ws).expect("sa");
    assert_eq!(sa.metadata.name.as_deref(), Some("waddle-prod"));
    assert_eq!(sa.automount_service_account_token, Some(false));

    let cm = build_config_map(&ws, &bindings).expect("configmap");
    let cm_data = cm.data.as_ref().expect("cm data");
    assert_eq!(
        cm_data.get("WADDLE_SPICEDB_INSECURE").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        cm_data.get("WADDLE_S2S_CONFIG_FILE").map(String::as_str),
        Some("/etc/waddle/s2s/s2s.toml")
    );
    assert_eq!(
        cm_data.get("WADDLE_S3_BUCKET").map(String::as_str),
        Some("waddle-uploads")
    );
    assert_eq!(
        cm_data.get("WADDLE_HTTP_ADDR").map(String::as_str),
        Some("0.0.0.0:3000")
    );
    let cm_digest = config_map_digest(&cm).expect("digest");

    let dep = build_deployment(&ws, &validated, &bindings, &cm_digest).expect("deployment");
    let dep_spec = dep.spec.as_ref().expect("dep spec");
    assert_eq!(
        dep_spec.replicas, None,
        "omitted spec.replicas must stay None for HPA compatibility"
    );

    let pod_spec = dep_spec.template.spec.as_ref().expect("pod spec");
    assert_eq!(
        pod_spec.termination_grace_period_seconds,
        Some(validated.termination_grace_period_seconds)
    );
    let container = &pod_spec.containers[0];
    let sec_ctx = container.security_context.as_ref().expect("sec ctx");
    assert_eq!(sec_ctx.run_as_non_root, Some(true));
    assert_eq!(sec_ctx.run_as_user, Some(1000));
    assert_eq!(sec_ctx.allow_privilege_escalation, Some(false));

    let env_from_secrets: Vec<&str> = container
        .env_from
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|ef| ef.secret_ref.as_ref().map(|sr| sr.name.as_str()))
        .collect();
    assert!(env_from_secrets.contains(&"waddle-prod-runtime-secrets"));
    assert!(env_from_secrets.contains(&"waddle-extra-env"));
    assert!(
        !env_from_secrets.contains(&"waddle-apns"),
        "APNs key MUST NOT be injected via envFrom"
    );

    let env_map: BTreeMap<&str, &k8s_openapi::api::core::v1::EnvVar> = container
        .env
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|ev| (ev.name.as_str(), ev))
        .collect();
    let db_url = env_map["WADDLE_DATABASE_URL"]
        .value
        .as_deref()
        .expect("db url value");
    assert!(db_url.contains("sslmode=verify-full"));
    assert!(db_url.contains("sslrootcert=/etc/waddle/postgres-ca/ca.crt"));
    assert!(db_url.contains("search_path%3Dwaddle"));
    assert_eq!(
        env_map["WADDLE_APNS_KEY_PATH"].value.as_deref(),
        Some("/var/run/secrets/waddle-apns/AuthKey.p8")
    );
    assert_eq!(
        env_map["WADDLE_CLUSTERING_KEYPAIR_POOL"]
            .value_from
            .as_ref()
            .and_then(|vf| vf.secret_key_ref.as_ref())
            .map(|skr| skr.name.as_str()),
        Some("waddle-keypool")
    );

    let apns_vol = pod_spec
        .volumes
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .find(|v| v.name == "apns-auth-key")
        .expect("apns volume");
    assert_eq!(
        apns_vol.secret.as_ref().and_then(|s| s.default_mode),
        Some(0o440)
    );

    assert!(build_http_service(&ws).is_ok());
    assert!(build_headless_service(&ws).expect("headless").is_some());
    assert!(build_s2s_service(&ws).expect("s2s").is_some());
    assert!(build_pdb(&ws).expect("pdb").is_some());
    let np = build_network_policy(&ws)
        .expect("netpol")
        .expect("netpol present");
    assert_eq!(np.metadata.name.as_deref(), Some("waddle-prod-swarm"));
    let allowed_ports: Vec<i32> = np
        .spec
        .as_ref()
        .and_then(|s| s.ingress.as_ref())
        .into_iter()
        .flatten()
        .flat_map(|r| r.ports.as_deref().unwrap_or(&[]))
        .filter_map(|p| p.port.as_ref())
        .filter_map(|p| match p {
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(v) => Some(*v),
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(_) => None,
        })
        .collect();
    assert!(allowed_ports.contains(&ws.spec.service.http_port));
    assert!(allowed_ports.contains(&7900));
    assert!(
        allowed_ports.contains(&5269),
        "clustered NetworkPolicy must allow S2S ingress port 5269, ports={allowed_ports:?}"
    );
    assert!(build_pvc(&ws).expect("pvc").is_some());

    let mut no_s2s_ws = ws.clone();
    no_s2s_ws.spec.s2s.enabled = false;
    let np_no_s2s = build_network_policy(&no_s2s_ws)
        .expect("netpol no s2s")
        .expect("netpol present");
    let ports_no_s2s: Vec<i32> = np_no_s2s
        .spec
        .as_ref()
        .and_then(|s| s.ingress.as_ref())
        .into_iter()
        .flatten()
        .flat_map(|r| r.ports.as_deref().unwrap_or(&[]))
        .filter_map(|p| p.port.as_ref())
        .filter_map(|p| match p {
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(v) => Some(*v),
            k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(_) => None,
        })
        .collect();
    assert!(!ports_no_s2s.contains(&5269));

    let expected_hash = compute_workload_hash(&ws, dep_spec).expect("workload hash");
    assert_eq!(
        dep.metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(ANNOTATION_SPEC_HASH))
            .map(String::as_str),
        Some(expected_hash.as_str())
    );

    let mut service_edited_ws = ws.clone();
    service_edited_ws.spec.service.http_port = 8081;
    let custom_port_cm = build_config_map(&service_edited_ws, &bindings).expect("custom port cm");
    assert_eq!(
        custom_port_cm
            .data
            .as_ref()
            .and_then(|d| d.get("WADDLE_HTTP_ADDR"))
            .map(String::as_str),
        Some("0.0.0.0:8081")
    );

    let mut rolling_ws = ws.clone();
    rolling_ws.spec.update_strategy = waddle_operator_crd::UpdateStrategyKind::RollingUpdate;
    let rolling_val = validate_spec(&rolling_ws.spec).expect("rolling spec valid with RWX+S3");
    let rolling_dep = build_deployment(&rolling_ws, &rolling_val, &bindings, &cm_digest)
        .expect("rolling deployment");
    let rolling_cfg = rolling_dep
        .spec
        .as_ref()
        .and_then(|s| s.strategy.as_ref())
        .and_then(|s| s.rolling_update.as_ref())
        .expect("rollingUpdate strategy parameters must be populated");
    assert_eq!(
        rolling_cfg.max_surge,
        Some(k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(1))
    );
    assert_eq!(
        rolling_cfg.max_unavailable,
        Some(k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(0))
    );

    let service_changed_hash =
        build_deployment(&service_edited_ws, &validated, &bindings, &cm_digest)
            .expect("hash 2")
            .metadata
            .annotations
            .expect("ann 2")
            .get(ANNOTATION_SPEC_HASH)
            .cloned();
    assert_ne!(
        dep.metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(ANNOTATION_SPEC_HASH))
            .cloned(),
        service_changed_hash,
        "editing companion service port must change ANNOTATION_SPEC_HASH"
    );
}

#[test]
fn managed_external_secret_and_yolo_secret_preserve_keys_and_redact() {
    let ws = sample_eso_clustered_server();
    let es_v1 = build_managed_external_secret(&ws, DiscoveredEsoVersion::V1).expect("es v1");
    let es_v1beta1 =
        build_managed_external_secret(&ws, DiscoveredEsoVersion::V1Beta1).expect("es v1beta1");
    assert!(managed_external_secret_drifted(&es_v1, &es_v1beta1));
    assert!(!managed_external_secret_drifted(&es_v1, &es_v1));

    let mut live_with_eso_defaults = es_v1.clone();
    live_with_eso_defaults.data["spec"]["target"]["deletionPolicy"] = json!("Retain");
    if let Some(entries) = live_with_eso_defaults.data["spec"]["data"].as_array_mut() {
        for item in entries {
            item["remoteRef"]["conversionStrategy"] = json!("Default");
            item["remoteRef"]["decodingStrategy"] = json!("None");
            item["remoteRef"]["metadataPolicy"] = json!("None");
        }
    }
    assert!(
        !managed_external_secret_drifted(&live_with_eso_defaults, &es_v1),
        "ESO CRD schema defaults must not register as drift"
    );

    let mut real_drift = live_with_eso_defaults;
    real_drift.data["spec"]["refreshInterval"] = json!("5m");
    assert!(
        managed_external_secret_drifted(&real_drift, &es_v1),
        "changing refreshInterval must be detected as drift"
    );

    let yolo_ws: WaddleServer = serde_json::from_value(json!({
        "apiVersion": "waddle.social/v1alpha1",
        "kind": "WaddleServer",
        "metadata": {
            "name": "quick",
            "namespace": "dev",
            "uid": "22222222-3333-4444-8555-666666666666"
        },
        "spec": {
            "baseUrl": "https://quick.local",
            "xmpp": { "domain": "quick.local" },
            "postgres": {
                "mode": "CloudNativePg",
                "cnpg": { "clusterName": "pg" }
            },
            "spicedb": {
                "mode": "SpiceDbOperator",
                "operator": { "clusterName": "spicedb" }
            },
            "secrets": { "mode": "Yolo" }
        }
    }))
    .expect("yolo spec");

    let mut sec = build_yolo_secret(&yolo_ws).expect("yolo secret");
    let original_data = sec.data.clone().expect("data map");
    assert!(original_data[KEY_SESSION_KEY].0.len() >= 32);
    assert!(original_data[KEY_OCCUPANT_ID_SECRET].0.len() >= 32);
    assert!(!original_data[KEY_DEPLOYMENT_UUID].0.is_empty());

    assert!(
        !fill_missing_yolo_keys(&yolo_ws, &mut sec).expect("no-op backfill"),
        "existing valid keys must never be rotated"
    );
    assert_eq!(sec.data.as_ref().expect("data"), &original_data);

    sec.data
        .as_mut()
        .expect("data")
        .insert(KEY_DEPLOYMENT_UUID.to_owned(), ByteString(Vec::new()));
    assert!(fill_missing_yolo_keys(&yolo_ws, &mut sec).expect("uuid backfilled"));
    assert_eq!(
        sec.data.as_ref().expect("data")[KEY_SESSION_KEY],
        original_data[KEY_SESSION_KEY],
        "session key must stay unchanged while backfilling missing UUID"
    );

    let secret_wrapper = generate_base64_secret_48().expect("csprng secret");
    let raw = secret_wrapper.expose_secret().to_owned();
    let redacted = RedactedSecret::new(raw.clone());
    assert!(!format!("{redacted:?}").contains(&raw));

    let k1 = generate_ed25519_seed_base64().expect("seed 1");
    let k2 = generate_ed25519_seed_base64().expect("seed 2");
    assert_eq!(
        validate_keypair_pool(&format!("{},{}", k1.expose_secret(), k2.expose_secret()), 2)
            .expect("2 valid seeds"),
        2
    );
}
