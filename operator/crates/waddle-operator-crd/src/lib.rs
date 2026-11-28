//! Custom Resource Definition types, schema generation, and pure spec validation
//! for `WaddleServer` and `WaddleTenant` Kubernetes resources (`waddle.social/v1alpha1`).

mod dependencies;
mod identifiers;
mod secrets;
mod server_config;
mod validation;
mod waddle_server;
mod waddle_tenant;

pub use dependencies::{
    AuthzBackendKind, AuthzSpec, BrokerKmsProviderKind, BrokerKmsSpec, BrokerSpec,
    CloudNativePgSource, ExtensionsSpec, ExternalPostgresSource, ExternalSpiceDbSource,
    LocalObjectRef, OpenFgaDependencySpec, PostgresDependencySpec, PostgresSourceMode,
    RestrictedRuntimeRolesSpec, RestrictedRuntimeSpec, ScalewayKmsSpec, SecretKeyRef,
    SpiceDbDependencySpec, SpiceDbOperatorSource, SpiceDbSourceMode, StorageBackendKind,
    StorageSpec, TursoStorageSpec, VaultKmsSpec,
};
pub use identifiers::{
    ContentDigest, DeploymentUuid, GitSha, IdentifierParseError, ImageDigest, PgIdentifier,
    ResourceName, XmppDomain,
};
pub use secrets::{
    ApnsSecretMountSpec, EsoApiVersion, EsoDataMapping, EsoRemoteKeyRef, EsoRemoteRefs,
    ExternalSecretsSpec, ManagedExternalSecretSpec, SecretProviderMode, SecretStoreKind,
    SecretStoreRef, SecretsSpec, YoloSecretsSpec,
};
pub use server_config::{
    ClusteringSpec, DeploymentIdentitySpec, GatewayRefSpec, HttpProbeSpec, ImageSpec, IngressSpec,
    NetworkPolicySpec, PersistenceAccessMode, PersistenceSpec, PodAntiAffinitySpec,
    PodDisruptionBudgetSpec, ProbesSpec, PullPolicy, ResourceRequirementsSpec, ResourceSpec,
    S2sServiceType, S2sSpec, S3StorageSpec, ServerMode, ServiceSpec, ServiceType, ShutdownSpec,
    TelemetrySpec, UpdateStrategyKind, WsKeepaliveSpec, XmppSpec, SHUTDOWN_KILL_MARGIN_SECS,
    SHUTDOWN_RETIRE_BUDGET_SECS, SHUTDOWN_TERMINAL_SWEEP_BUDGET_SECS,
};
pub use validation::{validate_spec, ValidatedSpec, ValidationReport, ValidationViolation};
pub use waddle_server::{
    DependenciesStatus, DependencyHealthPhase, DependencyStatusSummary, WaddleServer,
    WaddleServerPhase, WaddleServerSpec, WaddleServerStatus,
};
pub use waddle_tenant::{
    validate_tenant_ingress_against_server, validate_tenant_spec, ResolvedIngressMode,
    ResolvedTenantIngress, TenantAuthProvider, TenantAuthProviderKind, TenantAuthSpec,
    TenantExtensionsSpec, TenantIngressError, TenantIngressMode, TenantIngressSpec,
    TenantPlacementMode, TenantQuotasSpec, TenantValidationReport, TenantValidationViolation,
    WaddleTenant, WaddleTenantPhase, WaddleTenantSpec, WaddleTenantStatus,
    BUILTIN_RESERVED_SUBDOMAINS,
};
