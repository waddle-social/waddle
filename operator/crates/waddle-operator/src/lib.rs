//! Kubernetes controller, observers, and workload builders for `WaddleServer` and `WaddleTenant`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Resolved non-secret runtime bindings passed to manifest builders.
pub mod bindings;
/// Command-line flags and environment configuration for the operator binary.
pub mod cli;
/// Controller loop, watches, and single-resource reconciliation entrypoint.
pub mod controller;
/// Typed operator error definitions.
pub mod error;
/// Imperative command execution and diff-gated status persistence.
pub mod executor;
/// Third-party CRD capability discovery and health projections (CNPG, SpiceDB, ESO).
pub mod external_crds;
/// HTTP `/healthz`, `/readyz`, and `/metrics` server.
pub mod health;
/// Kubernetes Lease-based leader election.
pub mod leader_election;
/// Prometheus metrics registry for reconciliation and phase gauges.
pub mod metrics;
/// Cluster state observer converting API objects into a pure `ObservedWorld`.
pub mod observer;
/// OpenFGA HTTP provisioning client for tenant stores and authorization models.
pub mod openfga_client;
/// Kubernetes workload manifest builders (Deployment, ConfigMap, Services, PDB, NetworkPolicy, Secrets).
pub mod resources;
/// `WaddleTenant` reconciliation controller and projection ConfigMap manager.
pub mod tenant_controller;
/// Per-tenant Gateway API, cert-manager, and ExternalDNS resource builders and reconciler.
pub mod tenant_ingress;
/// Cryptographic generators, redacted secret wrappers, and keypool validators.
pub mod yolo;

pub use cli::{Cli, Command, CrdArgs, CrdKind, LogFormat, RunArgs};
pub use controller::{reconcile_single, run_controller, ControllerContext};
pub use error::OperatorError;
pub use health::HealthState;
pub use metrics::OperatorMetrics;
pub use observer::OperatorPolicy;
pub use tenant_controller::{reconcile_tenant, TenantControllerContext, TENANT_FINALIZER};
