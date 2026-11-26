use std::net::SocketAddr;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Log output format for the operator process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum LogFormat {
    /// Structured JSON logs for production log collectors.
    #[default]
    Json,
    /// Human-readable logs for local development.
    Pretty,
}

/// CRD kind emitted by `waddle-operator crd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum CrdKind {
    /// Emit `WaddleServer` CustomResourceDefinition YAML.
    #[default]
    Server,
    /// Emit `WaddleTenant` CustomResourceDefinition YAML.
    Tenant,
}

/// Arguments for `waddle-operator crd`.
#[derive(Debug, Clone, Copy, Args)]
pub struct CrdArgs {
    /// Which CustomResourceDefinition to render (`server` or `tenant`).
    #[arg(long, value_enum, default_value_t = CrdKind::Server)]
    pub kind: CrdKind,
}

/// Top-level CLI for `waddle-operator`.
#[derive(Debug, Parser)]
#[command(
    name = "waddle-operator",
    about = "Kubernetes Operator for deploying Waddle XMPP servers and tenants"
)]
pub struct Cli {
    /// Operator subcommand to execute.
    #[command(subcommand)]
    pub command: Command,
}

/// Supported operator commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the Kubernetes controller loop and health/metrics HTTP server.
    Run(RunArgs),
    /// Print CustomResourceDefinition YAML to stdout (`WaddleServer` by default).
    Crd(CrdArgs),
}

/// Configuration flags for `waddle-operator run`.
#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// Restrict watching to a single namespace (omit to watch all namespaces).
    #[arg(long, env = "WADDLE_OPERATOR_NAMESPACE")]
    pub namespace: Option<String>,

    /// Socket address for `/healthz`, `/readyz`, and `/metrics`.
    #[arg(
        long,
        env = "WADDLE_OPERATOR_METRICS_BIND",
        default_value = "0.0.0.0:8080"
    )]
    pub metrics_bind: SocketAddr,

    /// Log output format (`json` or `pretty`).
    #[arg(long, env = "WADDLE_OPERATOR_LOG_FORMAT", value_enum, default_value_t = LogFormat::Json)]
    pub log_format: LogFormat,

    /// Enable Kubernetes Lease-based leader election.
    #[arg(long, env = "WADDLE_OPERATOR_LEADER_ELECTION", default_value_t = true, action = clap::ArgAction::Set)]
    pub leader_election: bool,

    /// Name of the `coordination.k8s.io/v1` Lease resource used for leader election.
    #[arg(
        long,
        env = "WADDLE_OPERATOR_LEASE_NAME",
        default_value = "waddle-operator-leader"
    )]
    pub lease_name: String,

    /// Namespace holding the leader election Lease (defaults to pod namespace or `default`).
    #[arg(long, env = "WADDLE_OPERATOR_LEASE_NAMESPACE")]
    pub lease_namespace: Option<String>,

    /// Unique holder identity for leader election (defaults to pod name / hostname).
    #[arg(long, env = "WADDLE_OPERATOR_HOLDER_ID")]
    pub holder_id: Option<String>,

    /// Permit `spec.secrets.mode: Yolo` secret generation (keep disabled in production).
    #[arg(long, env = "WADDLE_OPERATOR_ENABLE_YOLO_MODE", default_value_t = false, action = clap::ArgAction::Set)]
    pub enable_yolo_mode: bool,

    /// Comma-separated list of `ClusterSecretStore` names allowed for managed ExternalSecrets.
    #[arg(
        long,
        env = "WADDLE_OPERATOR_ALLOWED_CLUSTER_SECRET_STORES",
        value_delimiter = ','
    )]
    pub allowed_cluster_secret_stores: Vec<String>,
}
