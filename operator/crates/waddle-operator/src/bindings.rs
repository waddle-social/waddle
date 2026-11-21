use waddle_operator_crd::{ContentDigest, RestrictedRuntimeSpec};

use crate::external_crds::DiscoveredEsoVersion;

/// Resolved non-secret PostgreSQL credential and endpoint binding for pod manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostgresBinding {
    /// Connection string read from a Secret key via `secretKeyRef`.
    UriSecret {
        /// Same-namespace Secret name.
        secret_name: String,
        /// Key holding the Postgres URI.
        uri_key: String,
    },
    /// Credentials read from a `kubernetes.io/basic-auth` Secret and expanded into a URI.
    BasicAuthSecret {
        /// Same-namespace Secret name.
        secret_name: String,
        /// Key holding the database username.
        username_key: String,
        /// Key holding the database password.
        password_key: String,
        /// Service DNS host (e.g. `<cluster>-rw`).
        host: String,
        /// PostgreSQL port.
        port: u16,
        /// Database name.
        database: String,
    },
    /// Restricted PostgreSQL runtime binding over verified TLS + handoff ConfigMap.
    RestrictedRuntime {
        /// Full restricted runtime settings.
        spec: Box<RestrictedRuntimeSpec>,
    },
}

/// Resolved non-secret SpiceDB endpoint and preshared-key reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpiceDbBinding {
    /// SpiceDB gRPC endpoint URL (`http://...` or `https://...`).
    pub endpoint: String,
    /// Value for `WADDLE_SPICEDB_INSECURE`.
    pub insecure: bool,
    /// Same-namespace Secret holding `WADDLE_SPICEDB_PRESHARED_KEY`.
    pub preshared_key_secret_name: String,
    /// Key inside `preshared_key_secret_name`.
    pub preshared_key_secret_key: String,
}

/// Non-secret cluster bindings passed from the observer to workload builders.
///
/// This type holds only Kubernetes resource names, keys, endpoint strings, and SHA-256 digests;
/// raw secret values NEVER appear here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRuntimeBindings {
    /// PostgreSQL wiring for the `waddle-server` container.
    pub postgres: PostgresBinding,
    /// SpiceDB wiring for the `waddle-server` container.
    pub spicedb: SpiceDbBinding,
    /// Same-namespace Secret providing `WADDLE_SESSION_KEY` and `WADDLE_OCCUPANT_ID_SECRET`.
    pub runtime_secret_name: String,
    /// Additional same-namespace Secrets mounted via `envFrom`.
    pub extra_secret_refs: Vec<String>,
    /// Optional separate Secret providing `WADDLE_CLUSTERING_KEYPAIR_POOL`.
    pub clustering_keypool_secret_name: Option<String>,
    /// Resolved `WADDLE_DEPLOYMENT_UUID` (from spec in ESO mode or YOLO secret).
    pub deployment_uuid: Option<String>,
    /// Optional one-shot `WADDLE_DB_LINEAGE_ACTION`.
    pub lineage_action: Option<String>,
    /// SHA-256 digest of consumed secrets for pod template rollout checksums.
    pub secrets_checksum: Option<ContentDigest>,
    /// Fingerprinted SHA-256 hash of `WADDLE_SESSION_KEY` for rotation detection.
    pub session_key_fingerprint: Option<ContentDigest>,
    /// Fingerprinted SHA-256 hash of `WADDLE_OCCUPANT_ID_SECRET` for rotation detection.
    pub occupant_id_fingerprint: Option<ContentDigest>,
    /// Discovered ESO API version when managing an `ExternalSecret`.
    pub eso_api_version: Option<DiscoveredEsoVersion>,
}
