use serde::Serialize;

use crate::{ContractError, Qualification};

use super::declaration::{LocalIsolationManifest, LocalPurpose};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Requirement {
    Required,
    Forbidden,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ManagementOperation {
    InspectStatus,
    RequestShutdown,
    ForceStop,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct ManagementRequirements {
    private_run_owned_local_socket: Requirement,
    run_directory_and_socket_ownership_validation: Requirement,
    separate_guest_serial: Requirement,
    serial_is_untrusted: Requirement,
    serial_capture_limit_bytes: u32,
    tcp_listener: Requirement,
    allowed_operations: [ManagementOperation; 3],
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct IsolationRequirements {
    guest_disk: Requirement,
    kvm_acceleration: Requirement,
    software_emulation: Requirement,
    unprivileged_qemu: Requirement,
    implicit_devices: Requirement,
    user_configuration: Requirement,
    network_interfaces: Requirement,
    host_shares: Requirement,
    host_runtime_sockets: Requirement,
    device_passthrough: Requirement,
    vsock: Requirement,
    independent_host_confinement: Requirement,
    management: ManagementRequirements,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Verification {
    Unverified,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ExecutionAuthorization {
    NotAuthorized,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum InventoryScope {
    DeclarationOnly,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct Assurance {
    execution_authorization: ExecutionAuthorization,
    artifact_digest_verification: Verification,
    artifact_closure: Verification,
    architecture_compatibility: Verification,
    host_confinement: Verification,
    isolation: Verification,
    inventory_scope: InventoryScope,
    qualification: Qualification,
}

/// A deterministic proposal, not a launcher configuration or execution authority.
/// It cannot be deserialized or constructed without validating the declaration.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct LocalIsolationPlan {
    manifest: LocalIsolationManifest,
    requirements: IsolationRequirements,
    assurance: Assurance,
}

impl LocalIsolationPlan {
    pub fn build(mut manifest: LocalIsolationManifest) -> Result<Self, ContractError> {
        manifest.validate()?;
        manifest.artifacts.sort_by_key(|artifact| artifact.role);
        manifest
            .additional_artifacts
            .sort_by_key(|artifact| artifact.id);
        Ok(Self {
            requirements: IsolationRequirements {
                guest_disk: match manifest.purpose {
                    LocalPurpose::IsolationProbe => Requirement::Forbidden,
                    LocalPurpose::Qualification => Requirement::Required,
                },
                kvm_acceleration: Requirement::Required,
                software_emulation: Requirement::Forbidden,
                unprivileged_qemu: Requirement::Required,
                implicit_devices: Requirement::Forbidden,
                user_configuration: Requirement::Forbidden,
                network_interfaces: Requirement::Forbidden,
                host_shares: Requirement::Forbidden,
                host_runtime_sockets: Requirement::Forbidden,
                device_passthrough: Requirement::Forbidden,
                vsock: Requirement::Forbidden,
                independent_host_confinement: Requirement::Required,
                management: ManagementRequirements {
                    private_run_owned_local_socket: Requirement::Required,
                    run_directory_and_socket_ownership_validation: Requirement::Required,
                    separate_guest_serial: Requirement::Required,
                    serial_is_untrusted: Requirement::Required,
                    serial_capture_limit_bytes: 1_048_576,
                    tcp_listener: Requirement::Forbidden,
                    allowed_operations: [
                        ManagementOperation::InspectStatus,
                        ManagementOperation::RequestShutdown,
                        ManagementOperation::ForceStop,
                    ],
                },
            },
            manifest,
            assurance: Assurance {
                execution_authorization: ExecutionAuthorization::NotAuthorized,
                artifact_digest_verification: Verification::Unverified,
                artifact_closure: Verification::Unverified,
                architecture_compatibility: Verification::Unverified,
                host_confinement: Verification::Unverified,
                isolation: Verification::Unverified,
                inventory_scope: InventoryScope::DeclarationOnly,
                qualification: Qualification::NotAssessed,
            },
        })
    }

    pub fn manifest(&self) -> &LocalIsolationManifest {
        &self.manifest
    }
}
