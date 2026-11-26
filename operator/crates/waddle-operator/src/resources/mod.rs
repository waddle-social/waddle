//! Kubernetes resource builders for owned `WaddleServer` and `WaddleTenant` child manifests.

mod config_map;
mod deployment;
mod external_secret;
mod labels;
mod network_policy;
mod pdb;
mod pvc;
mod secret;
mod service;
mod service_account;

pub use config_map::{
    build_config_map, build_tenants_config_map, config_map_digest, TENANTS_MOUNT_PATH,
};
pub use deployment::{
    build_deployment, compute_workload_hash, ANNOTATION_OCCUPANT_ID_FINGERPRINT,
    ANNOTATION_SESSION_KEY_FINGERPRINT, ANNOTATION_SPEC_HASH,
};
pub use external_secret::{build_managed_external_secret, managed_external_secret_drifted};
pub use labels::{
    common_labels, config_map_name, headless_service_name, managed_external_secret_name,
    managed_target_secret_name, network_policy_name, owner_reference, pvc_name, s2s_service_name,
    selector_labels, tenant_name, tenant_namespace, tenant_owner_reference,
    tenant_projection_config_map_name, tenants_config_map_name, tenants_config_map_name_for_server,
    workload_name, workload_namespace, yolo_secret_name, MANAGED_BY_LABEL, MANAGED_BY_VALUE,
    SERVER_LABEL, TENANT_LABEL,
};
pub use network_policy::build_network_policy;
pub use pdb::build_pdb;
pub use pvc::build_pvc;
pub use secret::{
    build_yolo_secret, fill_missing_yolo_keys, KEY_DEPLOYMENT_UUID, KEY_OCCUPANT_ID_SECRET,
    KEY_SESSION_KEY, KEY_SPICEDB_PRESHARED_KEY,
};
pub use service::{build_headless_service, build_http_service, build_s2s_service};
pub use service_account::build_service_account;
