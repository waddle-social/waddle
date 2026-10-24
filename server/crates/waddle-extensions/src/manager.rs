//! Extension loading, invocation, message processing, and room-observer orchestration.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use futures::future::join_all;
use hmac::{Hmac, KeyInit, Mac};
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
}

/// Number of enrichment entries appended to a stanza and effects requiring host handling.
#[derive(Debug, Clone, Default)]
pub struct MessageExtensionOutcome {
    /// Number of enrichment entries appended to the processed message.
    pub enrichments_added: usize,
    /// Host operations requested by the extension response.
    pub effects: Vec<ExtensionEffect>,
}

/// Result of invoking one explicitly selected message observer.
#[derive(Debug, Clone, Default)]
pub struct ObserverOutcome {
    /// Host operations requested by the extension response.
    pub effects: Vec<ExtensionEffect>,
}

/// Owned launch submission plus signed context and token used to authorize guest dispatch.
pub struct LaunchInvocationRequest<'a> {
    /// Identifier of the loaded plugin selected for this invocation.
    pub plugin_name: &'a str,
    /// Identifier of the action associated with this value.
    pub action_id: &'a str,
    /// Identifier of the launch associated with this value.
    pub launch_id: LaunchId,
    /// Community and source-message context bound to the launch authorization.
    pub context: LaunchContext,
    /// Full JID of the authenticated client submitting the launch.
    pub requester: FullJidValue,
    /// Identifier of the session associated with this value.
    pub session_id: Option<CommandSessionId>,
    /// Action requested by the client or exposed by the UI.
    pub action: Option<CommandAction>,
    /// Fields carried by the data form or provider payload.
    pub fields: Vec<crate::types::FormFieldValue>,
    /// Optional structured form supplied with the command action.
    pub form: Option<crate::types::DataForm>,
    /// Optional RFC 3339 deadline bound into the signed launch token.
    pub expires_at: Option<crate::types::Timestamp>,
    /// Signed token authorizing this exact launch context and submitted payload fields.
    pub launch_token: &'a str,
}

/// Borrowed launch parameters used to verify the submitted token without invoking a guest.
pub struct LaunchValidationRequest<'a> {
    /// Identifier of the loaded plugin selected for this invocation.
    pub plugin_name: &'a str,
    /// Identifier of the action associated with this value.
    pub action_id: &'a str,
    /// Identifier of the launch associated with this value.
    pub launch_id: &'a LaunchId,
    /// Community and source-message context bound to the launch authorization.
    pub context: &'a LaunchContext,
    /// Fields carried by the data form or provider payload.
    pub fields: &'a [crate::types::FormFieldValue],
    /// Optional RFC 3339 deadline bound into the signed launch token.
    pub expires_at: Option<&'a crate::types::Timestamp>,
    /// Signed token authorizing this exact launch context and submitted payload fields.
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

/// Ad-hoc command submission and requester authority dispatched by the extension manager.
pub struct CommandInvocationRequest<'a> {
    /// PubSub or discovery node selected by the request.
    pub node: &'a str,
    /// Identifier of the Waddle community providing invocation context.
    pub waddle_id: WaddleId,
    /// Bare JID of the room in scope.
    pub room: Option<RoomJid>,
    /// Full JID of the authenticated client submitting the command.
    pub requester: FullJidValue,
    /// Identifier of the session associated with this value.
    pub session_id: Option<CommandSessionId>,
    /// Action requested by the client or exposed by the UI.
    pub action: Option<CommandAction>,
    /// Fields carried by the data form or provider payload.
    pub fields: Vec<crate::types::FormFieldValue>,
    /// Optional structured form supplied with the command action.
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

/// Loaded extension actors, discovery metadata, and launch-signing authority for one host.
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
#[cfg(any(test, feature = "test-support"))]
mod observer_test_support;
mod room_observation;
mod signing;

use launch_signing::*;
use manifest::*;
use message_context::*;
use message_helpers::*;

#[cfg(test)]
mod tests;
