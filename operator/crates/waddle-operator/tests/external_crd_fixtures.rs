//! Golden JSON fixture tests for external CRD health and version projections.

use serde_json::json;
use waddle_operator::external_crds::{
    evaluate_cnpg_health, evaluate_eso_status, evaluate_spicedb_health, CnpgClusterView,
    ExternalSecretView, SpiceDbClusterView,
};
use waddle_operator_fsm::DependencyHealth;

#[test]
fn cnpg_ready_fixture_and_defaults() {
    let payload = json!({
        "apiVersion": "postgresql.cnpg.io/v1",
        "kind": "Cluster",
        "metadata": { "name": "waddle-pg", "namespace": "waddle" },
        "spec": {
            "instances": 3,
            "bootstrap": {
                "initdb": {
                    "database": "waddle_prod"
                }
            }
        },
        "status": {
            "readyInstances": 3,
            "currentPrimary": "waddle-pg-1",
            "phase": "Cluster in healthy state",
            "conditions": [
                {
                    "type": "Ready",
                    "status": "True",
                    "reason": "ClusterIsReady",
                    "message": "Cluster is Ready"
                }
            ]
        }
    });

    let view: CnpgClusterView = serde_json::from_value(payload).expect("valid CNPG fixture");
    assert_eq!(view.resolved_secret_name("waddle-pg"), "waddle-pg-app");
    assert_eq!(view.resolved_database_name(), "waddle_prod");
    assert_eq!(
        evaluate_cnpg_health(&view, "waddle-pg", true),
        DependencyHealth::Ready {
            endpoint_hint: "waddle-pg-rw".to_owned(),
        }
    );
}

#[test]
fn cnpg_custom_secret_and_unhealthy_branches() {
    let payload = json!({
        "spec": {
            "bootstrap": {
                "initdb": {
                    "secret": { "name": "pg-custom-creds" }
                }
            }
        },
        "status": {
            "readyInstances": 0,
            "currentPrimary": "",
            "phase": "Setting up primary",
            "conditions": [
                {
                    "type": "Ready",
                    "status": "False",
                    "reason": "ClusterIsNotReady",
                    "message": "0 of 1 instances ready"
                }
            ]
        }
    });

    let view: CnpgClusterView = serde_json::from_value(payload).expect("valid CNPG fixture");
    assert_eq!(view.resolved_secret_name("pg"), "pg-custom-creds");
    assert_eq!(view.resolved_database_name(), "app");

    assert_eq!(
        evaluate_cnpg_health(&view, "pg", true),
        DependencyHealth::NotReady {
            reason: "ClusterIsNotReady".to_owned(),
            detail: "0 of 1 instances ready".to_owned(),
        }
    );

    assert_eq!(
        evaluate_cnpg_health(&view, "pg", false),
        DependencyHealth::Ready {
            endpoint_hint: "pg-rw".to_owned(),
        }
    );
}

#[test]
fn spicedb_ready_paused_rolling_and_memory_fixtures() {
    let ready_tls = json!({
        "metadata": { "generation": 4 },
        "spec": {
            "secretName": "spicedb-preshared-key",
            "config": {
                "datastoreEngine": "postgres",
                "tlsSecretName": "spicedb-grpc-tls"
            }
        },
        "status": {
            "observedGeneration": 4,
            "currentMigrationHash": "abc123",
            "targetMigrationHash": "abc123",
            "conditions": [
                {
                    "type": "ConfigurationWarning",
                    "status": "True",
                    "reason": "DeprecatedFlag",
                    "message": "non-blocking advisory warning"
                }
            ]
        }
    });

    let view: SpiceDbClusterView = serde_json::from_value(ready_tls).expect("valid SpiceDBCluster");
    assert!(view.has_tls());
    assert_eq!(
        evaluate_spicedb_health(&view, "https://spicedb:50051", 2, true),
        DependencyHealth::Ready {
            endpoint_hint: "https://spicedb:50051".to_owned(),
        }
    );
    assert!(matches!(
        evaluate_spicedb_health(&view, "https://spicedb:50051", 0, true),
        DependencyHealth::NotReady { ref reason, .. } if reason == "NoReadyEndpoints"
    ));

    let rolling_with_paused = json!({
        "metadata": { "generation": 4 },
        "spec": {
            "secretName": "spicedb-key",
            "config": { "datastoreEngine": "postgres" }
        },
        "status": {
            "observedGeneration": 4,
            "currentMigrationHash": "abc123",
            "targetMigrationHash": "abc123",
            "conditions": [
                {
                    "type": "RollingDeployment",
                    "status": "True",
                    "message": "rolling out new replicas"
                }
            ]
        }
    });

    let rolling_view: SpiceDbClusterView =
        serde_json::from_value(rolling_with_paused).expect("rolling view");
    assert!(!rolling_view.has_tls());
    assert!(evaluate_spicedb_health(&rolling_view, "http://spicedb:50051", 1, true).is_ready());
    assert!(matches!(
        evaluate_spicedb_health(&rolling_view, "http://spicedb:50051", 0, true),
        DependencyHealth::NotReady { ref reason, .. } if reason == "RollingDeployment"
    ));

    let paused = json!({
        "metadata": { "generation": 1 },
        "spec": { "config": { "datastoreEngine": "memory" } },
        "status": {
            "observedGeneration": 1,
            "conditions": [
                { "type": "Paused", "status": "True", "message": "operator paused" }
            ]
        }
    });
    let paused_view: SpiceDbClusterView = serde_json::from_value(paused).expect("paused view");
    assert!(matches!(
        evaluate_spicedb_health(&paused_view, "http://spicedb:50051", 1, true),
        DependencyHealth::NotReady { ref reason, .. } if reason == "Paused"
    ));

    let healthy_memory = json!({
        "metadata": { "generation": 1 },
        "spec": { "config": { "datastoreEngine": "memory" } },
        "status": { "observedGeneration": 1, "conditions": [] }
    });
    let mem_view: SpiceDbClusterView = serde_json::from_value(healthy_memory).expect("memory view");
    assert!(evaluate_spicedb_health(&mem_view, "http://spicedb:50051", 1, true).is_ready());
}

#[test]
fn eso_v1_and_v1beta1_status_fixtures() {
    let es_v1 = json!({
        "apiVersion": "external-secrets.io/v1",
        "kind": "ExternalSecret",
        "spec": { "target": { "name": "waddle-runtime" } },
        "status": {
            "conditions": [
                {
                    "type": "Ready",
                    "status": "True",
                    "reason": "SecretSynced",
                    "message": "Secret was synced"
                }
            ]
        }
    });
    let v1_view: ExternalSecretView = serde_json::from_value(es_v1).expect("ESO v1 view");
    assert_eq!(
        v1_view.spec.target.as_ref().and_then(|t| t.name.as_deref()),
        Some("waddle-runtime")
    );
    assert_eq!(evaluate_eso_status(&v1_view), (Some(true), None));

    let es_v1beta1_err = json!({
        "apiVersion": "external-secrets.io/v1beta1",
        "kind": "ExternalSecret",
        "status": {
            "conditions": [
                {
                    "type": "Ready",
                    "status": "False",
                    "reason": "SecretSyncedError",
                    "message": "could not get secret data from provider"
                }
            ]
        }
    });
    let beta_view: ExternalSecretView =
        serde_json::from_value(es_v1beta1_err).expect("ESO v1beta1 error view");
    assert_eq!(
        evaluate_eso_status(&beta_view),
        (
            Some(false),
            Some("could not get secret data from provider".to_owned())
        )
    );
}
