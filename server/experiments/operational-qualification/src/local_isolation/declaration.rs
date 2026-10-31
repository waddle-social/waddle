use std::collections::BTreeSet;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::ContractError;

use super::object::deserialize_object;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct LocalRunId(pub u32);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GuestArchitecture {
    X86_64,
    Aarch64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalPurpose {
    IsolationProbe,
    Qualification,
}

impl LocalPurpose {
    /// Minimum declared inventory only; not proof of the transitive closure.
    pub fn required_roles(self) -> &'static [ArtifactRole] {
        match self {
            Self::IsolationProbe => &[
                ArtifactRole::HostQemuBinary,
                ArtifactRole::GuestKernel,
                ArtifactRole::GuestInitrd,
            ],
            Self::Qualification => &ArtifactRole::ALL,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct LocalResources {
    pub vcpus: u16,
    pub memory_mib: u32,
    pub disk_gib: u32,
}

impl<'de> Deserialize<'de> for LocalResources {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            vcpus: u16,
            memory_mib: u32,
            disk_gib: u32,
        }
        let Fields {
            vcpus,
            memory_mib,
            disk_gib,
        } = deserialize_object(deserializer)?;
        Ok(Self {
            vcpus,
            memory_mib,
            disk_gib,
        })
    }
}

/// A non-sentinel digest declaration, never evidence of verified content.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Sha256Digest(String);

impl FromStr for Sha256Digest {
    type Err = ContractError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = ContractError::LocalIsolation(
            "digest requires sha256: and 64 lowercase hexadecimal digits, excluding all-zero",
        );
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(invalid);
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || hex.bytes().all(|byte| byte == b'0')
        {
            return Err(invalid);
        }
        Ok(Self(value.to_owned()))
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactContent {
    FileBytes { digest: Sha256Digest },
    OciManifest { digest: Sha256Digest },
}

impl<'de> Deserialize<'de> for ArtifactContent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum Fields {
            FileBytes { digest: Sha256Digest },
            OciManifest { digest: Sha256Digest },
        }
        match deserialize_object(deserializer)? {
            Fields::FileBytes { digest } => Ok(Self::FileBytes { digest }),
            Fields::OciManifest { digest } => Ok(Self::OciManifest { digest }),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRole {
    HostQemuBinary,
    GuestKernel,
    GuestInitrd,
    GuestDisk,
    KindBinary,
    KindNode,
    Cni,
    CnpgOperator,
    Postgres,
    Spicedb,
    ObjectStore,
    Colony,
    Oidc,
    TelemetryCollector,
    TelemetryStore,
    Waddle,
    Generator,
}

impl ArtifactRole {
    pub const ALL: [Self; 17] = [
        Self::HostQemuBinary,
        Self::GuestKernel,
        Self::GuestInitrd,
        Self::GuestDisk,
        Self::KindBinary,
        Self::KindNode,
        Self::Cni,
        Self::CnpgOperator,
        Self::Postgres,
        Self::Spicedb,
        Self::ObjectStore,
        Self::Colony,
        Self::Oidc,
        Self::TelemetryCollector,
        Self::TelemetryStore,
        Self::Waddle,
        Self::Generator,
    ];

    fn accepts(self, content: &ArtifactContent) -> bool {
        match self {
            Self::HostQemuBinary
            | Self::GuestKernel
            | Self::GuestInitrd
            | Self::GuestDisk
            | Self::KindBinary => matches!(content, ArtifactContent::FileBytes { .. }),
            Self::KindNode | Self::CnpgOperator => {
                matches!(content, ArtifactContent::OciManifest { .. })
            }
            Self::Cni
            | Self::Postgres
            | Self::Spicedb
            | Self::ObjectStore
            | Self::Colony
            | Self::Oidc
            | Self::TelemetryCollector
            | Self::TelemetryStore
            | Self::Waddle
            | Self::Generator => matches!(
                content,
                ArtifactContent::FileBytes { .. } | ArtifactContent::OciManifest { .. }
            ),
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DeclaredArtifact {
    pub role: ArtifactRole,
    pub architecture: GuestArchitecture,
    pub content: ArtifactContent,
}

impl<'de> Deserialize<'de> for DeclaredArtifact {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            role: ArtifactRole,
            architecture: GuestArchitecture,
            content: ArtifactContent,
        }
        let Fields {
            role,
            architecture,
            content,
        } = deserialize_object(deserializer)?;
        Ok(Self {
            role,
            architecture,
            content,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct AdditionalArtifactId(pub u16);

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdditionalArtifactRole {
    HostRuntimeFile,
    GuestRuntimeFile,
    OperatorImage,
    ServiceImage,
    OciBlob,
    OfflineConfiguration,
}

impl AdditionalArtifactRole {
    fn accepts(self, content: &ArtifactContent) -> bool {
        match self {
            Self::OperatorImage | Self::ServiceImage => {
                matches!(content, ArtifactContent::OciManifest { .. })
            }
            Self::HostRuntimeFile
            | Self::GuestRuntimeFile
            | Self::OciBlob
            | Self::OfflineConfiguration => matches!(content, ArtifactContent::FileBytes { .. }),
        }
    }

    fn allowed_in_probe(self) -> bool {
        matches!(self, Self::HostRuntimeFile | Self::GuestRuntimeFile)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AdditionalArtifact {
    pub id: AdditionalArtifactId,
    pub role: AdditionalArtifactRole,
    pub architecture: GuestArchitecture,
    pub content: ArtifactContent,
}

impl<'de> Deserialize<'de> for AdditionalArtifact {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            id: AdditionalArtifactId,
            role: AdditionalArtifactRole,
            architecture: GuestArchitecture,
            content: ArtifactContent,
        }
        let Fields {
            id,
            role,
            architecture,
            content,
        } = deserialize_object(deserializer)?;
        Ok(Self {
            id,
            role,
            architecture,
            content,
        })
    }
}

/// No locations, endpoints, credentials, host paths or executable arguments exist.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct LocalIsolationManifest {
    pub schema_version: u16,
    pub run_id: LocalRunId,
    pub purpose: LocalPurpose,
    pub architecture: GuestArchitecture,
    pub resources: LocalResources,
    pub artifacts: Vec<DeclaredArtifact>,
    pub additional_artifacts: Vec<AdditionalArtifact>,
}

impl<'de> Deserialize<'de> for LocalIsolationManifest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            schema_version: u16,
            run_id: LocalRunId,
            purpose: LocalPurpose,
            architecture: GuestArchitecture,
            resources: LocalResources,
            artifacts: Vec<DeclaredArtifact>,
            additional_artifacts: Vec<AdditionalArtifact>,
        }
        let Fields {
            schema_version,
            run_id,
            purpose,
            architecture,
            resources,
            artifacts,
            additional_artifacts,
        } = deserialize_object(deserializer)?;
        Ok(Self {
            schema_version,
            run_id,
            purpose,
            architecture,
            resources,
            artifacts,
            additional_artifacts,
        })
    }
}

impl LocalIsolationManifest {
    pub(super) fn validate(&self) -> Result<(), ContractError> {
        let invalid = ContractError::LocalIsolation;
        if self.schema_version != 1 || self.run_id.0 == 0 {
            return Err(invalid(
                "schema_version must be 1 and numeric run_id must be positive",
            ));
        }
        let resources = &self.resources;
        match self.purpose {
            LocalPurpose::IsolationProbe
                if !(1..=2).contains(&resources.vcpus)
                    || !(128..=1_024).contains(&resources.memory_mib)
                    || resources.disk_gib != 0 =>
            {
                return Err(invalid(
                    "probe requires 1..=2 vCPUs, 128..=1024 MiB memory and no disk",
                ));
            }
            LocalPurpose::Qualification
                if !(1..=16).contains(&resources.vcpus)
                    || !(1_024..=49_152).contains(&resources.memory_mib)
                    || !(8..=128).contains(&resources.disk_gib) =>
            {
                return Err(invalid(
                    "qualification requires 1..=16 vCPUs, 1024..=49152 MiB memory and 8..=128 GiB disk",
                ));
            }
            _ => {}
        }
        let required = self.purpose.required_roles();
        if self.artifacts.len() != required.len() || self.additional_artifacts.len() > 64 {
            return Err(invalid(
                "requires the purpose's exact minimum roles and at most 64 additional artifacts",
            ));
        }
        let mut roles = BTreeSet::new();
        for artifact in &self.artifacts {
            if !roles.insert(artifact.role) || !required.contains(&artifact.role) {
                return Err(invalid(
                    "required artifact roles must be unique and match purpose",
                ));
            }
            if artifact.architecture != self.architecture {
                return Err(invalid(
                    "artifact architecture must match guest declaration",
                ));
            }
            if !artifact.role.accepts(&artifact.content) {
                return Err(invalid("artifact content type does not match its role"));
            }
        }
        let mut ids = BTreeSet::new();
        for artifact in &self.additional_artifacts {
            if artifact.id.0 == 0 || !ids.insert(artifact.id) {
                return Err(invalid(
                    "additional artifact IDs must be positive and unique",
                ));
            }
            if artifact.architecture != self.architecture
                || !artifact.role.accepts(&artifact.content)
            {
                return Err(invalid(
                    "additional artifact architecture or content type is invalid",
                ));
            }
            if self.purpose == LocalPurpose::IsolationProbe && !artifact.role.allowed_in_probe() {
                return Err(invalid(
                    "probe permits only additional host or guest runtime files",
                ));
            }
        }
        Ok(())
    }
}
