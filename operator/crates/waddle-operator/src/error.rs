/// Errors surfaced by the Waddle Kubernetes operator.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OperatorError {
    /// Kubernetes API client or watch failure.
    #[error("kubernetes API error during {context}: {source}")]
    Kube {
        /// Non-secret description of the failed API operation.
        context: &'static str,
        /// Underlying Kubernetes client error.
        #[source]
        source: kube::Error,
    },
    /// Resource is missing required metadata (name or namespace).
    #[error("WaddleServer is missing required metadata field {field}")]
    MissingMetadata {
        /// Name of the missing metadata field.
        field: &'static str,
    },
    /// CSPRNG generation failure.
    #[error("failed to read OS CSPRNG bytes for secret generation")]
    RandomGeneration,
    /// Invalid or malformed external dependency or workload state.
    #[error("invalid cluster state: {0}")]
    InvalidClusterState(String),
    /// Serialization error for CRD or child manifests.
    #[error("serialization error: {0}")]
    Serialization(String),
    /// Failed to bind or serve the HTTP health/metrics endpoint.
    #[error("health/metrics HTTP server error: {0}")]
    HttpServer(#[from] std::io::Error),
    /// Leader lease was lost or could not be renewed.
    #[error("leader election lease lost or expired")]
    LeadershipLost,
}
