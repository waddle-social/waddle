use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use futures::future::join_all;
use hmac::{Hmac, Mac};
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use thiserror::Error;
use tokio::time::timeout;
use tracing::{debug, warn};
use xmpp_parsers::{jid::BareJid, message::Message};

use crate::actor::WasmExtensionActor;
use crate::config::{ExtensionConfig, ExtensionModuleConfig};
use crate::host_tools::{DenyingExtensionHostTools, ExtensionHostTools};
use crate::oci::OciExtensionPuller;
use crate::runtime::{LoadedExtension, WasmRuntime};
use crate::types::{
    is_official_namespace, message_has_framework_envelope, CommandAction, CommandInvocation,
    CommandNode, CommandSessionId, DetectedLink, DisplayText, ExtensionCapability, ExtensionEffect,
    ExtensionEnvelope, ExtensionEvent, ExtensionManifest, FullJidValue, LaunchContext, LaunchId,
    LaunchInvocation, LinkTarget, MessageContext, MessageHook, PayloadNamespace, PluginId,
    ProviderWebhook, ReplyTarget, RoomJid, StanzaId, ThreadId, WaddleId, FRAMEWORK_NAMESPACE,
};

const MAX_DETECTED_LINKS: usize = 3;
const EXTENSION_ENRICH_TIMEOUT: Duration = Duration::from_millis(750);
const EXTENSION_OBSERVE_TIMEOUT: Duration = Duration::from_secs(45);
const EXTENSION_COMMAND_TIMEOUT: Duration = Duration::from_secs(45);
const EXTENSION_PROVIDER_WEBHOOK_TIMEOUT: Duration = Duration::from_secs(45);
const XEP_0359_STANZA_ID_NS: &str = "urn:xmpp:sid:0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageHookMode {
    All,
    EnrichOnly,
    ObserveOnly,
}

#[derive(Debug, Clone, Default)]
pub struct MessageExtensionOutcome {
    pub enrichments_added: usize,
    pub effects: Vec<ExtensionEffect>,
}

pub struct LaunchInvocationRequest<'a> {
    pub plugin_name: &'a str,
    pub action_id: &'a str,
    pub launch_id: LaunchId,
    pub context: LaunchContext,
    pub requester: FullJidValue,
    pub session_id: Option<CommandSessionId>,
    pub action: Option<CommandAction>,
    pub fields: Vec<crate::types::FormFieldValue>,
    pub form: Option<crate::types::DataForm>,
    pub expires_at: Option<crate::types::Timestamp>,
    pub launch_token: &'a str,
}

pub struct LaunchValidationRequest<'a> {
    pub plugin_name: &'a str,
    pub action_id: &'a str,
    pub launch_id: &'a LaunchId,
    pub context: &'a LaunchContext,
    pub fields: &'a [crate::types::FormFieldValue],
    pub expires_at: Option<&'a crate::types::Timestamp>,
    pub launch_token: &'a str,
}

struct LaunchTokenVerification<'a> {
    plugin: &'a PluginId,
    action: &'a crate::types::ActionId,
    launch_id: &'a LaunchId,
    context: &'a LaunchContext,
    fields: &'a [crate::types::FormFieldValue],
    expires_at: Option<&'a crate::types::Timestamp>,
    token: &'a str,
}

pub struct CommandInvocationRequest<'a> {
    pub node: &'a str,
    pub waddle_id: WaddleId,
    pub room: Option<RoomJid>,
    pub requester: FullJidValue,
    pub session_id: Option<CommandSessionId>,
    pub action: Option<CommandAction>,
    pub fields: Vec<crate::types::FormFieldValue>,
    pub form: Option<crate::types::DataForm>,
}

#[derive(Debug, Error)]
enum EffectiveModuleConfigError {
    #[error("extension {extension} config_secret_files requires config to be a JSON object")]
    NonObjectBaseConfig { extension: String },
    #[error("failed to read config_secret_files[{key}] from {path} for extension {extension}")]
    ReadSecretFile {
        extension: String,
        key: String,
        path: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub struct ExtensionManager {
    actors: Vec<Arc<WasmExtensionActor>>,
    feature_namespaces: Vec<String>,
    route_descriptors: Vec<crate::types::ExtensionRouteDescriptor>,
    launch_signing_key: Option<Vec<u8>>,
}

mod construction;
mod invocation;
mod launch_signing;
mod manifest;
mod message_context;
mod message_helpers;
mod message_processing;
mod signing;

use launch_signing::*;
use manifest::*;
use message_context::*;
use message_helpers::*;

#[cfg(test)]
mod tests;
