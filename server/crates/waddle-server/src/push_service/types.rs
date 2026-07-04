//! Typed domain model for the Push Service: nodes, devices,
//! registrations, fan-out results, delivery attempts, and publish jobs.

use std::fmt;

use jid::BareJid;
use waddle_xmpp::XmppError;

pub(super) const NODE_STATUS_ACTIVE: &str = "active";

pub(super) const NODE_STATUS_DISABLED: &str = "disabled";

pub(super) const DEVICE_STATUS_ACTIVE: &str = "active";

pub(super) const DEVICE_STATUS_DISABLED: &str = "disabled";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushDevicePlatform {
    Web,
    Apns,
    Fcm,
}

impl PushDevicePlatform {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Apns => "apns",
            Self::Fcm => "fcm",
        }
    }

    pub fn parse(value: &str) -> Result<Self, XmppError> {
        match value {
            "web" => Ok(Self::Web),
            "apns" => Ok(Self::Apns),
            "fcm" => Ok(Self::Fcm),
            other => Err(XmppError::bad_request(Some(format!(
                "Unsupported push device platform '{other}'"
            )))),
        }
    }
}

impl fmt::Display for PushDevicePlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PushNodeStatus {
    Active,
    Disabled,
}

impl PushNodeStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Active => NODE_STATUS_ACTIVE,
            Self::Disabled => NODE_STATUS_DISABLED,
        }
    }

    pub(super) fn parse(value: &str) -> Result<Self, XmppError> {
        match value {
            NODE_STATUS_ACTIVE => Ok(Self::Active),
            NODE_STATUS_DISABLED => Ok(Self::Disabled),
            other => Err(XmppError::internal(format!(
                "Invalid push node status '{other}'"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PushServiceNode {
    pub(super) node: String,
    pub(super) owner_bare_jid: BareJid,
    pub(super) app_id: String,
    pub(super) status: PushNodeStatus,
    pub(super) created_at_ms: i64,
    pub(super) updated_at_ms: i64,
}

impl PushServiceNode {
    pub fn node(&self) -> &str {
        &self.node
    }

    pub fn owner_bare_jid(&self) -> &BareJid {
        &self.owner_bare_jid
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    pub fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }

    pub fn updated_at_ms(&self) -> i64 {
        self.updated_at_ms
    }
}

#[derive(Clone)]
pub struct PushDeviceRegistration {
    pub(super) device_id: String,
    pub(super) node: String,
    pub(super) platform: PushDevicePlatform,
    pub(super) environment: String,
    pub(super) provider_endpoint: Option<String>,
    pub(super) provider_token: Option<String>,
    pub(super) provider_key_material: Option<String>,
}

impl PushDeviceRegistration {
    pub fn new(
        device_id: impl Into<String>,
        node: impl Into<String>,
        platform: PushDevicePlatform,
        environment: impl Into<String>,
    ) -> Self {
        Self {
            device_id: device_id.into(),
            node: node.into(),
            platform,
            environment: environment.into(),
            provider_endpoint: None,
            provider_token: None,
            provider_key_material: None,
        }
    }

    pub fn with_provider_endpoint(mut self, provider_endpoint: Option<String>) -> Self {
        self.provider_endpoint = provider_endpoint;
        self
    }

    pub fn with_provider_token(mut self, provider_token: Option<String>) -> Self {
        self.provider_token = provider_token;
        self
    }

    pub fn with_provider_key_material(mut self, provider_key_material: Option<String>) -> Self {
        self.provider_key_material = provider_key_material;
        self
    }
}

impl fmt::Debug for PushDeviceRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushDeviceRegistration")
            .field("device_id", &self.device_id)
            .field("node", &self.node)
            .field("platform", &self.platform)
            .field("environment", &self.environment)
            .field(
                "provider_endpoint",
                &self.provider_endpoint.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "provider_token",
                &self.provider_token.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "provider_key_material",
                &self.provider_key_material.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct PushServiceDevice {
    pub(super) device_id: String,
    pub(super) node: String,
    pub(super) platform: PushDevicePlatform,
    pub(super) environment: String,
    pub(super) provider_endpoint: Option<String>,
    #[cfg(test)]
    pub(super) provider_token: Option<String>,
    #[cfg(test)]
    pub(super) provider_key_material: Option<String>,
}

impl PushServiceDevice {
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    pub fn provider_endpoint(&self) -> Option<&str> {
        self.provider_endpoint.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn provider_token(&self) -> Option<&str> {
        self.provider_token.as_deref()
    }

    pub fn platform(&self) -> PushDevicePlatform {
        self.platform
    }

    pub fn environment(&self) -> &str {
        &self.environment
    }

    #[cfg(test)]
    pub(crate) fn provider_key_material(&self) -> Option<&str> {
        self.provider_key_material.as_deref()
    }
}

#[derive(Debug, Clone)]
pub struct PushFanoutResult {
    pub(super) item_id: String,
    pub(super) attempted_devices: usize,
}

impl PushFanoutResult {
    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    pub fn attempted_devices(&self) -> usize {
        self.attempted_devices
    }
}

#[derive(Debug, Clone)]
pub struct PushDeliveryAttempt {
    pub(super) attempt_id: String,
    pub(super) node: String,
    pub(super) device_id: String,
    pub(super) item_id: String,
    pub(super) status: String,
}

impl PushDeliveryAttempt {
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    pub fn status(&self) -> &str {
        &self.status
    }
}

#[derive(Debug, Clone)]
pub struct PushPublishJob {
    pub(super) job_id: String,
    pub(super) owner_bare_jid: BareJid,
    pub(super) node: String,
    pub(super) item_id: String,
    pub(super) push_service_jid: Option<String>,
    pub(super) status: String,
    /// The UUID-string written by phase 1's claim. Phase 3's UPDATE
    /// gates on this so a stale-claim recovery + concurrent re-claim
    /// can't persist attempts from the original worker.
    pub(super) claim_token: String,
}

impl PushPublishJob {
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub(super) fn owner_bare_jid(&self) -> &BareJid {
        &self.owner_bare_jid
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    pub(super) fn push_service_jid(&self) -> Option<&str> {
        self.push_service_jid.as_deref()
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub(super) fn claim_token(&self) -> &str {
        &self.claim_token
    }
}

#[derive(Debug, Clone)]
pub struct PushPublishJobEnqueue {
    pub(super) item_id: String,
    pub(super) queued: bool,
}

impl PushPublishJobEnqueue {
    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    pub fn queued(&self) -> bool {
        self.queued
    }
}
