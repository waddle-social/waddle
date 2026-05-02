use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use futures::future::join_all;
use hmac::{Hmac, Mac};
use regex::Regex;
use serde_json::Value;
use sha2::Sha256;
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
    ReplyTarget, RoomJid, StanzaId, ThreadId, WaddleId, FRAMEWORK_NAMESPACE,
};

const MAX_DETECTED_LINKS: usize = 3;
const EXTENSION_ENRICH_TIMEOUT: Duration = Duration::from_millis(750);
const EXTENSION_OBSERVE_TIMEOUT: Duration = Duration::from_secs(45);
const EXTENSION_COMMAND_TIMEOUT: Duration = Duration::from_secs(45);
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

pub struct CommandInvocationRequest<'a> {
    pub node: &'a str,
    pub waddle_id: WaddleId,
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
    launch_signing_key: Option<Vec<u8>>,
}

impl ExtensionManager {
    /// Build an `ExtensionManager` from the given configuration.
    ///
    /// Configured extension modules fail fast. Message enrichment itself remains
    /// fail-open so user messages are not lost after startup.
    pub async fn from_config(config: ExtensionConfig) -> Result<Self> {
        Self::from_config_with_host_tools(config, Arc::new(DenyingExtensionHostTools)).await
    }

    pub async fn from_config_with_host_tools(
        config: ExtensionConfig,
        host_tools: Arc<dyn ExtensionHostTools>,
    ) -> Result<Self> {
        if !config.enabled {
            return Ok(Self {
                actors: Vec::new(),
                feature_namespaces: Vec::new(),
                launch_signing_key: None,
            });
        }
        config.validate().map_err(anyhow::Error::msg)?;

        let runtime = WasmRuntime::new()?;
        let puller = OciExtensionPuller::new(&config.cache_dir);
        let mut actors = Vec::new();
        let mut feature_namespaces = Vec::new();
        let mut plugin_ids = HashSet::new();
        let mut command_nodes = HashSet::new();
        let mut payload_namespaces: HashMap<PayloadNamespace, PluginId> = HashMap::new();

        for module in &config.modules {
            let config_json = match effective_module_config_json(module) {
                Ok(config_json) => config_json,
                Err(error) => {
                    return Err(anyhow::Error::new(error).context(format!(
                        "failed to prepare extension config for {}",
                        module.name
                    )));
                }
            };

            let wasm_path = match puller.resolve_wasm_path(module).await {
                Ok(path) => path,
                Err(error) => {
                    return Err(error.context(format!(
                        "failed to resolve extension WASM path for {}",
                        module.name
                    )));
                }
            };

            let loaded = match LoadedExtension::load(&runtime, &wasm_path) {
                Ok(loaded) => loaded,
                Err(error) => {
                    if module.local_path.is_none() {
                        remove_invalid_cached_extension(module, &wasm_path);
                    }
                    return Err(error.context(format!(
                        "failed to compile extension component for {}",
                        module.name
                    )));
                }
            };

            let actor = match WasmExtensionActor::initialize(loaded, &config_json).await {
                Ok(actor) => actor,
                Err(error) => {
                    return Err(
                        error.context(format!("extension init() failed for {}", module.name))
                    );
                }
            }
            .with_host_tools(Arc::clone(&host_tools));

            let manifest = actor.manifest();
            validate_manifest_against_module(module, &manifest)?;
            validate_ai_chatbot_runtime_config(module, &manifest, &config_json)?;
            let actor = actor
                .with_grants(runtime_grants_for_module(module, &manifest))
                .with_allowed_http_origins(module.allowed_http_origins.clone());
            if !plugin_ids.insert(manifest.id.clone()) {
                bail!(
                    "extension plugin id {} is declared by multiple modules",
                    manifest.id
                );
            }
            for command in &manifest.commands {
                if command.node == CommandNode::invoke() {
                    continue;
                }
                if !command_nodes.insert(command.node.clone()) {
                    bail!(
                        "extension command node {} is declared by multiple modules",
                        command.node
                    );
                }
            }
            for rule in &manifest.payloads {
                match payload_namespaces.get(&rule.root.namespace) {
                    Some(owner) if owner != &manifest.id => {
                        bail!(
                            "extension payload namespace {} is declared by multiple modules",
                            rule.root.namespace
                        );
                    }
                    Some(_) => {}
                    None => {
                        payload_namespaces.insert(rule.root.namespace.clone(), manifest.id.clone());
                    }
                }
                push_feature_namespace(
                    module,
                    &mut feature_namespaces,
                    rule.root.namespace.as_str(),
                );
            }

            actors.push(Arc::new(actor));
        }

        Ok(Self {
            actors,
            feature_namespaces,
            launch_signing_key: None,
        })
    }

    pub fn with_launch_signing_key(mut self, key: impl AsRef<[u8]>) -> Self {
        let key = key.as_ref();
        if !key.is_empty() {
            self.launch_signing_key = Some(key.to_vec());
        }
        self
    }

    pub async fn from_env() -> Result<Self> {
        let config = ExtensionConfig::from_env().map_err(anyhow::Error::msg)?;
        Self::from_config(config).await
    }

    pub fn feature_namespaces(&self) -> &[String] {
        &self.feature_namespaces
    }

    pub fn extension_features(&self) -> Vec<String> {
        self.feature_namespaces.clone()
    }

    pub fn command_nodes(&self) -> Vec<(String, String)> {
        self.actors
            .iter()
            .filter(|actor| actor.has_grant(ExtensionCapability::Commands))
            .flat_map(|actor| {
                actor
                    .manifest()
                    .commands
                    .into_iter()
                    .filter(|command| command.node != CommandNode::invoke())
                    .map(|command| (command.node.into_string(), command.name.into_string()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub async fn invoke_command(
        &self,
        request: CommandInvocationRequest<'_>,
    ) -> Vec<ExtensionEffect> {
        let CommandInvocationRequest {
            node,
            waddle_id,
            requester,
            session_id,
            action,
            fields,
            form,
        } = request;
        let Ok(command_node) = CommandNode::new(node.to_string()) else {
            return Vec::new();
        };
        let dispatch_node = command_node.clone();
        let event = ExtensionEvent::Command(CommandInvocation {
            waddle_id,
            requester,
            command_node: dispatch_node,
            session_id,
            action,
            form,
            fields,
        });
        for actor in &self.actors {
            if actor.has_grant(ExtensionCapability::Commands)
                && actor.manifest().declares_command(&command_node)
            {
                return match timeout(EXTENSION_COMMAND_TIMEOUT, actor.handle_event(event)).await {
                    Ok(effects) => self.sign_effects(effects),
                    Err(_) => vec![ExtensionEffect::HostWarning(
                        DisplayText::new(format!("Extension command {command_node} timed out"))
                            .expect("timeout warning is non-empty"),
                    )],
                };
            }
        }
        Vec::new()
    }

    pub async fn invoke_launch(
        &self,
        request: LaunchInvocationRequest<'_>,
    ) -> Vec<ExtensionEffect> {
        let LaunchInvocationRequest {
            plugin_name,
            action_id,
            launch_id,
            context,
            requester,
            session_id,
            action,
            fields,
            form,
            expires_at,
            launch_token,
        } = request;
        let Ok(plugin_id) = PluginId::new(plugin_name.to_string()) else {
            return Vec::new();
        };
        let Ok(action_id) = crate::types::ActionId::new(action_id.to_string()) else {
            return Vec::new();
        };
        if !self.verify_launch_token(
            &plugin_id,
            &action_id,
            &launch_id,
            &context,
            expires_at.as_ref(),
            launch_token,
        ) {
            warn!(
                plugin = %plugin_name,
                launch_id = %launch_id,
                "rejected unsigned or tampered extension launch invocation"
            );
            return Vec::new();
        }
        let event = ExtensionEvent::Launch(LaunchInvocation {
            context,
            requester,
            launch_id,
            session_id,
            action,
            form,
            fields,
        });
        for actor in &self.actors {
            if actor.manifest().id.as_str() == plugin_name {
                return self.sign_effects(actor.handle_event(event).await);
            }
        }
        Vec::new()
    }

    pub fn validates_launch_invocation(
        &self,
        plugin_name: &str,
        action_id: &str,
        launch_id: &LaunchId,
        context: &LaunchContext,
        expires_at: Option<&crate::types::Timestamp>,
        launch_token: &str,
    ) -> bool {
        let Ok(plugin_id) = PluginId::new(plugin_name.to_string()) else {
            return false;
        };
        let Ok(action_id) = crate::types::ActionId::new(action_id.to_string()) else {
            return false;
        };
        self.verify_launch_token(
            &plugin_id,
            &action_id,
            launch_id,
            context,
            expires_at,
            launch_token,
        )
    }

    pub async fn enrich_message(&self, msg: &mut Message) -> usize {
        self.enrich_message_for_waddle(msg, WaddleId::new("local").expect("static waddle id"))
            .await
    }

    pub async fn enrich_message_for_waddle(&self, msg: &mut Message, waddle_id: WaddleId) -> usize {
        self.process_message_for_waddle(msg, waddle_id)
            .await
            .enrichments_added
    }

    pub async fn process_message_for_waddle(
        &self,
        msg: &mut Message,
        waddle_id: WaddleId,
    ) -> MessageExtensionOutcome {
        self.process_message_for_waddle_with_requester(msg, waddle_id, None)
            .await
    }

    pub async fn process_message_for_waddle_with_requester(
        &self,
        msg: &mut Message,
        waddle_id: WaddleId,
        requester: Option<BareJid>,
    ) -> MessageExtensionOutcome {
        self.process_message_for_waddle_with_requester_and_mode(
            msg,
            waddle_id,
            requester,
            MessageHookMode::All,
        )
        .await
    }

    pub async fn process_message_enrichments_for_waddle_with_requester(
        &self,
        msg: &mut Message,
        waddle_id: WaddleId,
        requester: Option<BareJid>,
    ) -> MessageExtensionOutcome {
        self.process_message_for_waddle_with_requester_and_mode(
            msg,
            waddle_id,
            requester,
            MessageHookMode::EnrichOnly,
        )
        .await
    }

    pub async fn process_message_observers_for_waddle_with_requester(
        &self,
        msg: &mut Message,
        waddle_id: WaddleId,
        requester: Option<BareJid>,
    ) -> MessageExtensionOutcome {
        self.process_message_for_waddle_with_requester_and_mode(
            msg,
            waddle_id,
            requester,
            MessageHookMode::ObserveOnly,
        )
        .await
    }

    async fn process_message_for_waddle_with_requester_and_mode(
        &self,
        msg: &mut Message,
        waddle_id: WaddleId,
        requester: Option<BareJid>,
        mode: MessageHookMode,
    ) -> MessageExtensionOutcome {
        if mode != MessageHookMode::ObserveOnly && message_has_framework_envelope(msg) {
            return MessageExtensionOutcome::default();
        }

        let Some(body) = msg
            .bodies
            .get("")
            .or_else(|| msg.bodies.values().next())
            .map(|body| body.0.clone())
        else {
            return MessageExtensionOutcome::default();
        };
        let Ok(body_text) = DisplayText::new(body.clone()) else {
            return MessageExtensionOutcome::default();
        };

        let links = detect_links(&body);

        let mut outcome = MessageExtensionOutcome::default();
        if !self.actors.is_empty() {
            let hook_links: Vec<LinkTarget> = links
                .into_iter()
                .filter_map(|link| LinkTarget::try_from(link).ok())
                .collect();
            let room = msg
                .to
                .as_ref()
                .or(msg.from.as_ref())
                .and_then(|jid| RoomJid::new(jid.to_bare().to_string()).ok());
            let source_stanza_id = room
                .as_ref()
                .and_then(|room| room_stanza_id_from_payloads(msg, room.as_str()))
                .or_else(|| msg.id.clone().and_then(|id| StanzaId::new(id).ok()));
            let sender = msg
                .from
                .as_ref()
                .and_then(|jid| FullJidValue::new(jid.to_string()).ok());
            let event = ExtensionEvent::MessageHook(MessageHook {
                context: MessageContext {
                    waddle_id: waddle_id.clone(),
                    stanza_id: source_stanza_id.clone(),
                    room,
                    sender,
                    thread_id: thread_id_from_message(msg),
                    reply_to: reply_target_from_payloads(&msg.payloads),
                },
                body: body_text,
                links: hook_links,
            });
            let enrich_futures = self.actors.iter().filter_map(|actor| {
                let manifest = actor.manifest();
                let declares_enrich =
                    manifest.declares_capability(crate::types::ExtensionCapability::MessageEnrich);
                let declares_observe =
                    manifest.declares_capability(crate::types::ExtensionCapability::MessageObserve);
                let grants_enrich =
                    actor.has_grant(crate::types::ExtensionCapability::MessageEnrich);
                let grants_observe =
                    actor.has_grant(crate::types::ExtensionCapability::MessageObserve);
                let selected = match mode {
                    MessageHookMode::All => {
                        (declares_enrich && grants_enrich) || (declares_observe && grants_observe)
                    }
                    MessageHookMode::EnrichOnly => {
                        declares_enrich && grants_enrich && !(declares_observe && grants_observe)
                    }
                    MessageHookMode::ObserveOnly => declares_observe && grants_observe,
                };
                if !selected {
                    return None;
                }
                let actor_name = actor.manifest().id.to_string();
                let manifest = actor.manifest();
                let actor = Arc::clone(actor);
                let event = event.clone();
                let waddle_id = waddle_id.clone();
                let requester = requester.clone();
                Some(async move {
                    let timeout_duration = if manifest
                        .declares_capability(crate::types::ExtensionCapability::MessageObserve)
                    {
                        EXTENSION_OBSERVE_TIMEOUT
                    } else {
                        EXTENSION_ENRICH_TIMEOUT
                    };
                    match timeout(
                        timeout_duration,
                        actor.handle_event_for_waddle_with_requester(event, waddle_id, requester),
                    )
                    .await
                    {
                        Ok(effects) => (actor_name, manifest, effects),
                        Err(_) => {
                            warn!(
                                extension = %actor_name,
                                timeout_secs = timeout_duration.as_secs(),
                                "extension message hook timed out; continuing fail-open"
                            );
                            (actor_name, manifest, Vec::new())
                        }
                    }
                })
            });
            let results = join_all(enrich_futures).await;

            let mut enrichments = Vec::new();
            let mut emitted_effects = Vec::new();
            for (actor_name, manifest, effects) in results {
                for effect in self.sign_effects(effects) {
                    if !effect.validate_for_manifest(&manifest) {
                        warn!(
                            extension = %actor_name,
                            "extension emitted undeclared or invalid message effect; dropping"
                        );
                        continue;
                    }
                    match effect {
                        ExtensionEffect::EnrichMessage(envelope) => {
                            enrichments.extend(envelope.enrichments);
                        }
                        ExtensionEffect::PublishPubSub(_)
                        | ExtensionEffect::ReferenceArtifact(_)
                        | ExtensionEffect::CommandForm(_) => {}
                        ExtensionEffect::HostWarning(warning) => {
                            emitted_effects.push(ExtensionEffect::HostWarning(warning));
                        }
                        ExtensionEffect::Noop => {}
                    }
                }
            }
            let count = enrichments.len();
            if !enrichments.is_empty() {
                msg.payloads
                    .push(ExtensionEnvelope::new(enrichments).to_minidom());
            }
            outcome.enrichments_added = count;
            outcome.effects = emitted_effects;
            if outcome.enrichments_added > 0 {
                debug!(
                    embeds_added = outcome.enrichments_added,
                    "message enriched by extensions"
                );
            }
        }
        outcome
    }

    fn sign_effects(&self, mut effects: Vec<ExtensionEffect>) -> Vec<ExtensionEffect> {
        for effect in &mut effects {
            match effect {
                ExtensionEffect::EnrichMessage(envelope) => {
                    for enrichment in &mut envelope.enrichments {
                        for launch in &mut enrichment.launches {
                            self.sign_launch(launch);
                        }
                    }
                }
                ExtensionEffect::PublishPubSub(_)
                | ExtensionEffect::ReferenceArtifact(_)
                | ExtensionEffect::CommandForm(_)
                | ExtensionEffect::HostWarning(_)
                | ExtensionEffect::Noop => {}
            }
        }
        effects
    }

    fn sign_launch(&self, launch: &mut crate::types::LaunchDescriptor) {
        let Some(key) = self.launch_signing_key.as_deref() else {
            return;
        };
        let expires_at = launch
            .expires_at
            .get_or_insert_with(|| default_launch_expiry().expect("generated expiry is valid"));
        let token = sign_launch_token(
            key,
            &launch.plugin,
            &launch.action,
            &launch.id,
            &launch.context,
            Some(expires_at),
        );
        launch.token = crate::types::LaunchToken::new(token).ok();
    }

    fn verify_launch_token(
        &self,
        plugin: &PluginId,
        action: &crate::types::ActionId,
        launch_id: &LaunchId,
        context: &LaunchContext,
        expires_at: Option<&crate::types::Timestamp>,
        token: &str,
    ) -> bool {
        let Some(key) = self.launch_signing_key.as_deref() else {
            return false;
        };
        if let Some(expires_at) = expires_at {
            let Ok(expires_at) = DateTime::parse_from_rfc3339(expires_at.as_str()) else {
                return false;
            };
            if expires_at.with_timezone(&Utc) <= Utc::now() {
                return false;
            }
        }
        let expected = sign_launch_token(key, plugin, action, launch_id, context, expires_at);
        constant_time_eq(expected.as_bytes(), token.as_bytes())
    }
}

fn sign_launch_token(
    key: &[u8],
    plugin: &PluginId,
    action: &crate::types::ActionId,
    launch_id: &LaunchId,
    context: &LaunchContext,
    expires_at: Option<&crate::types::Timestamp>,
) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(plugin.as_str().as_bytes());
    mac.update(b"\0");
    mac.update(action.as_str().as_bytes());
    mac.update(b"\0");
    mac.update(launch_id.as_str().as_bytes());
    mac.update(b"\0");
    mac.update(context.waddle_id.as_str().as_bytes());
    mac.update(b"\0");
    if let Some(stanza_id) = &context.source_stanza_id {
        mac.update(stanza_id.as_str().as_bytes());
    }
    mac.update(b"\0");
    if let Some(expires_at) = expires_at {
        mac.update(expires_at.as_str().as_bytes());
    }
    hex::encode(mac.finalize().into_bytes())
}

fn default_launch_expiry() -> Option<crate::types::Timestamp> {
    crate::types::Timestamp::new((Utc::now() + chrono::Duration::hours(1)).to_rfc3339()).ok()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |diff, (left, right)| diff | (left ^ right))
        == 0
}

fn validate_manifest_against_module(
    module: &ExtensionModuleConfig,
    manifest: &ExtensionManifest,
) -> Result<()> {
    if manifest.id.as_str() != module.name {
        bail!(
            "extension module {} returned manifest id {}; manifest id must match configured module name",
            module.name,
            manifest.id
        );
    }

    let expected_namespace = PayloadNamespace::new(module.namespace.clone()).map_err(|error| {
        anyhow::anyhow!("extension {} namespace is invalid: {error}", module.name)
    })?;
    for rule in &manifest.payloads {
        if rule.root.namespace != expected_namespace {
            bail!(
                "extension {} declared payload namespace {}; expected configured namespace {}",
                module.name,
                rule.root.namespace,
                expected_namespace
            );
        }
    }
    for node in &manifest.pubsub_nodes {
        if node.as_str() != expected_namespace.as_str()
            && !node
                .as_str()
                .strip_prefix(expected_namespace.as_str())
                .is_some_and(|suffix| suffix.starts_with(':'))
        {
            bail!(
                "extension {} declared PubSub node {} outside configured namespace {}",
                module.name,
                node,
                expected_namespace
            );
        }
    }
    for command in &manifest.commands {
        if command.node == CommandNode::invoke() {
            continue;
        }
        let expected_command = format!("{FRAMEWORK_NAMESPACE}:{}", manifest.id.as_str());
        if command.node.as_str() != expected_command {
            bail!(
                "extension {} declared command node {}; expected {}",
                module.name,
                command.node,
                expected_command
            );
        }
    }
    if manifest.id.as_str() == "ai-chatbot" {
        for capability in &manifest.capabilities {
            if !module.capability_grants.contains(capability) {
                bail!(
                    "extension {} requires explicit operator grant for declared capability {}",
                    module.name,
                    capability.as_str()
                );
            }
        }
    }
    Ok(())
}

fn runtime_grants_for_module(
    module: &ExtensionModuleConfig,
    manifest: &ExtensionManifest,
) -> HashSet<ExtensionCapability> {
    let declared = manifest
        .capabilities
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    module
        .capability_grants
        .iter()
        .copied()
        .filter(|capability| declared.contains(capability))
        .collect()
}

fn validate_ai_chatbot_runtime_config(
    module: &ExtensionModuleConfig,
    manifest: &ExtensionManifest,
    config_json: &str,
) -> Result<()> {
    if manifest.id.as_str() != "ai-chatbot" {
        return Ok(());
    }
    let config: Value = serde_json::from_str(config_json).map_err(|error| {
        anyhow::anyhow!(
            "extension {} provider config is invalid JSON: {error}",
            module.name
        )
    })?;
    let endpoint = required_ai_config_string(&module.name, &config, "endpoint")?;
    required_ai_config_string(&module.name, &config, "model")?;
    required_ai_config_string(&module.name, &config, "api_key")?;
    let endpoint_url = reqwest::Url::parse(endpoint).map_err(|error| {
        anyhow::anyhow!(
            "extension {} provider endpoint must be an absolute HTTPS URL: {error}",
            module.name
        )
    })?;
    if endpoint_url.scheme() != "https" {
        bail!("extension {} provider endpoint must use HTTPS", module.name);
    }
    let endpoint_origin = runtime_http_origin(&endpoint_url).ok_or_else(|| {
        anyhow::anyhow!(
            "extension {} provider endpoint must include a host",
            module.name
        )
    })?;
    if !module
        .allowed_http_origins
        .iter()
        .any(|origin| origin == &endpoint_origin)
    {
        bail!(
            "extension {} allowedHttpOrigins must include provider origin {}",
            module.name,
            endpoint_origin
        );
    }
    Ok(())
}

fn required_ai_config_string<'a>(
    module_name: &str,
    config: &'a Value,
    key: &str,
) -> Result<&'a str> {
    config
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("extension {module_name} provider config must set {key}"))
}

fn runtime_http_origin(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?;
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Some(format!("{}://{}{}", url.scheme(), host, port))
}

fn room_stanza_id_from_payloads(msg: &Message, room: &str) -> Option<StanzaId> {
    msg.payloads
        .iter()
        .find(|payload| {
            payload.name() == "stanza-id"
                && payload.ns() == XEP_0359_STANZA_ID_NS
                && payload.attr("by") == Some(room)
        })
        .and_then(|payload| payload.attr("id"))
        .and_then(|id| StanzaId::new(id.to_string()).ok())
}

fn push_feature_namespace(
    module: &ExtensionModuleConfig,
    feature_namespaces: &mut Vec<String>,
    namespace: &str,
) {
    if namespace.trim().is_empty() {
        return;
    }
    if is_official_namespace(namespace) {
        warn!(
            extension = %module.name,
            namespace,
            "extension attempted to advertise an official XMPP namespace; ignoring"
        );
        return;
    }
    if !namespace.starts_with("urn:") && !namespace.starts_with("https://") {
        warn!(
            extension = %module.name,
            namespace,
            "extension attempted to advertise a non-absolute namespace; ignoring"
        );
        return;
    }
    if !feature_namespaces.iter().any(|value| value == namespace) {
        feature_namespaces.push(namespace.to_string());
    }
}

fn remove_invalid_cached_extension(module: &ExtensionModuleConfig, wasm_path: &Path) {
    match std::fs::remove_file(wasm_path) {
        Ok(()) => {
            warn!(
                extension = %module.name,
                cache_path = %wasm_path.display(),
                "removed cached extension after component load failure"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            warn!(
                extension = %module.name,
                cache_path = %wasm_path.display(),
                %error,
                "failed to remove cached extension after component load failure"
            );
        }
    }
}

fn effective_module_config_json(
    module: &ExtensionModuleConfig,
) -> Result<String, EffectiveModuleConfigError> {
    effective_module_config_with_reader(module, |path| std::fs::read_to_string(path))
        .map(|value| value.to_string())
}

fn effective_module_config_with_reader<F>(
    module: &ExtensionModuleConfig,
    mut read_to_string: F,
) -> Result<Value, EffectiveModuleConfigError>
where
    F: FnMut(&Path) -> std::io::Result<String>,
{
    if module.config_secret_files.is_empty() {
        return Ok(module.config.clone());
    }

    let mut config = match module.config.clone() {
        Value::Object(config) => config,
        _ => {
            return Err(EffectiveModuleConfigError::NonObjectBaseConfig {
                extension: module.name.clone(),
            });
        }
    };

    for (key, path) in &module.config_secret_files {
        let contents = read_to_string(Path::new(path)).map_err(|source| {
            EffectiveModuleConfigError::ReadSecretFile {
                extension: module.name.clone(),
                key: key.clone(),
                path: path.clone(),
                source,
            }
        })?;
        config.insert(key.clone(), Value::String(contents));
    }

    Ok(Value::Object(config))
}

fn reply_target_from_payloads(payloads: &[minidom::Element]) -> Option<ReplyTarget> {
    payloads
        .iter()
        .find(|payload| payload.name() == "reply" && payload.ns() == "urn:xmpp:reply:0")
        .and_then(|payload| {
            let id = payload.attr("id").and_then(|id| StanzaId::new(id).ok())?;
            let to = payload.attr("to").and_then(|to| FullJidValue::new(to).ok());
            Some(ReplyTarget { id, to })
        })
}

fn thread_id_from_message(msg: &Message) -> Option<ThreadId> {
    msg.thread
        .as_ref()
        .and_then(|thread| ThreadId::new(thread.0.clone()).ok())
        .or_else(|| {
            msg.payloads
                .iter()
                .find(|payload| {
                    payload.name() == "thread-reply" && payload.ns() == "urn:waddle:forums:0"
                })
                .and_then(|payload| payload.attr("thread-id"))
                .and_then(|thread_id| ThreadId::new(thread_id).ok())
        })
}

fn detect_links(body: &str) -> Vec<DetectedLink> {
    static FENCED_CODE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static INLINE_CODE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static URL_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let fenced_re =
        FENCED_CODE_RE.get_or_init(|| Regex::new(r"(?s)```.*?```").expect("valid fenced regex"));
    let inline_re =
        INLINE_CODE_RE.get_or_init(|| Regex::new(r"`[^`\n]*`").expect("valid inline regex"));
    let re =
        URL_RE.get_or_init(|| Regex::new(r#"https?://[^\s<>"'`]+"#).expect("valid link regex"));

    let mut ignored_ranges: Vec<(usize, usize)> = fenced_re
        .find_iter(body)
        .map(|m| (m.start(), m.end()))
        .collect();
    ignored_ranges.extend(inline_re.find_iter(body).map(|m| (m.start(), m.end())));
    ignored_ranges.sort_unstable_by_key(|(start, _)| *start);

    let mut seen_urls = HashSet::new();
    let mut links = Vec::new();

    for m in re.find_iter(body) {
        if links.len() >= MAX_DETECTED_LINKS {
            break;
        }
        if ignored_ranges
            .iter()
            .any(|(start, end)| m.start() >= *start && m.start() < *end)
        {
            continue;
        }

        let trimmed = m
            .as_str()
            .trim_end_matches(['.', ',', '!', '?', ';', ':', ')', ']']);
        if trimmed.is_empty() || seen_urls.contains(trimmed) {
            continue;
        }
        seen_urls.insert(trimmed.to_string());

        links.push(DetectedLink {
            url: trimmed.to_string(),
            start_offset: m.start() as u32,
            end_offset: (m.start() + trimmed.len()) as u32,
        });
    }

    links
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use xmpp_parsers::message::{Body, Message};

    use super::{
        detect_links, effective_module_config_json, effective_module_config_with_reader,
        runtime_grants_for_module, EffectiveModuleConfigError, ExtensionManager,
        MAX_DETECTED_LINKS,
    };
    use crate::config::{ExtensionConfig, ExtensionModuleConfig};
    use crate::types::{DisplayText, ExtensionCapability, ExtensionManifest, PluginId};

    #[test]
    fn detects_urls() {
        let links = detect_links("hello https://github.com/waddle-social/waddle world");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://github.com/waddle-social/waddle");
    }

    #[test]
    fn deduplicates_and_caps_links() {
        let links = detect_links(
            "https://a.test https://a.test https://b.test https://c.test https://d.test",
        );
        assert_eq!(links.len(), MAX_DETECTED_LINKS);
        assert_eq!(links[0].url, "https://a.test");
        assert_eq!(links[1].url, "https://b.test");
    }

    #[test]
    fn skips_urls_inside_code_and_trims_punctuation() {
        let body = "Use `https://example.com/in-code` and:\nhttps://github.com/waddle-social/waddle).\n```https://skip.me```";
        let links = detect_links(body);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://github.com/waddle-social/waddle");
    }

    #[test]
    fn merges_secret_file_values_into_effective_config() {
        let mut config_secret_files = BTreeMap::new();
        config_secret_files.insert(
            "github_token".to_string(),
            "/secrets/github-token".to_string(),
        );
        config_secret_files.insert(
            "webhook_secret".to_string(),
            "/secrets/webhook-secret".to_string(),
        );

        let module = ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
            digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            ),
            tag: None,
            namespace: "urn:example:extension:1".to_string(),
            config: json!({
                "github_token": "from-config",
                "log_level": "debug"
            }),
            capability_grants: Vec::new(),
            allowed_http_origins: Vec::new(),
            config_secret_files,
            local_path: None,
        };

        let merged = effective_module_config_with_reader(&module, |path| match path.to_str() {
            Some("/secrets/github-token") => Ok("from-secret-file".to_string()),
            Some("/secrets/webhook-secret") => Ok("webhook-value".to_string()),
            other => panic!("unexpected path: {other:?}"),
        })
        .expect("config should merge");

        assert_eq!(
            merged,
            json!({
                "github_token": "from-secret-file",
                "log_level": "debug",
                "webhook_secret": "webhook-value"
            })
        );
    }

    #[test]
    fn rejects_non_object_config_when_secret_files_are_enabled() {
        let mut config_secret_files = BTreeMap::new();
        config_secret_files.insert(
            "github_token".to_string(),
            "/secrets/github-token".to_string(),
        );

        let module = ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
            digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            ),
            tag: None,
            namespace: "urn:example:extension:1".to_string(),
            config: json!(["not", "an", "object"]),
            capability_grants: Vec::new(),
            allowed_http_origins: Vec::new(),
            config_secret_files,
            local_path: None,
        };

        let error = effective_module_config_with_reader(&module, |_| Ok(String::new()))
            .expect_err("non-object config should fail");
        assert!(matches!(
            error,
            EffectiveModuleConfigError::NonObjectBaseConfig { extension }
            if extension == "example-extension"
        ));
    }

    #[test]
    fn reads_secret_files_from_disk_when_building_effective_config() {
        let artifact_dir = TestArtifacts::new();
        let secret_path = artifact_dir.write("github-token", "file-secret\n");

        let mut config_secret_files = BTreeMap::new();
        config_secret_files.insert(
            "github_token".to_string(),
            secret_path.to_string_lossy().into_owned(),
        );

        let module = ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
            digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            ),
            tag: None,
            namespace: "urn:example:extension:1".to_string(),
            config: json!({}),
            capability_grants: Vec::new(),
            allowed_http_origins: Vec::new(),
            config_secret_files,
            local_path: None,
        };

        let config_json =
            effective_module_config_json(&module).expect("secret file should be read from disk");
        assert_eq!(config_json, r#"{"github_token":"file-secret\n"}"#);
    }

    #[tokio::test]
    async fn from_config_fails_fast_when_configured_actor_cannot_load() {
        let config = ExtensionConfig {
            enabled: true,
            cache_dir: "/var/lib/waddle/extensions".to_string(),
            modules: vec![ExtensionModuleConfig {
                name: "example-extension".to_string(),
                registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
                digest: None,
                tag: Some("latest".to_string()),
                namespace: "urn:example:extension:1".to_string(),
                config: json!({}),
                capability_grants: Vec::new(),
                allowed_http_origins: Vec::new(),
                config_secret_files: Default::default(),
                local_path: Some("missing-example-extension-test.wasm".to_string()),
            }],
        };

        let error = ExtensionManager::from_config(config)
            .await
            .expect_err("configured extension load should fail fast");
        assert!(error
            .to_string()
            .contains("failed to resolve extension WASM path"));
    }

    #[tokio::test]
    async fn disabled_config_does_not_require_cache_dir() {
        let manager = ExtensionManager::from_config(ExtensionConfig {
            enabled: false,
            cache_dir: String::new(),
            modules: Vec::new(),
        })
        .await
        .expect("disabled extension manager should not validate unused cache dir");

        assert!(manager.feature_namespaces().is_empty());
    }

    #[test]
    fn advertised_feature_namespaces_reject_official_namespaces() {
        let module = ExtensionModuleConfig {
            name: "bad-advertiser".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/bad-advertiser".to_string(),
            digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            ),
            tag: None,
            namespace: "urn:waddle:bad:1".to_string(),
            config: json!({}),
            capability_grants: Vec::new(),
            allowed_http_origins: Vec::new(),
            config_secret_files: Default::default(),
            local_path: None,
        };
        let mut namespaces = Vec::new();

        super::push_feature_namespace(&module, &mut namespaces, "urn:xmpp:mam:2");
        super::push_feature_namespace(&module, &mut namespaces, "jabber:iq:roster");
        super::push_feature_namespace(
            &module,
            &mut namespaces,
            "http://jabber.org/protocol/disco#info",
        );
        super::push_feature_namespace(&module, &mut namespaces, "https://example.com/not-waddle");
        super::push_feature_namespace(&module, &mut namespaces, "urn:example:extension:1");
        super::push_feature_namespace(&module, &mut namespaces, "urn:example:extension:1");

        assert_eq!(
            namespaces,
            vec!["https://example.com/not-waddle", "urn:example:extension:1"]
        );
    }

    #[test]
    fn runtime_grants_are_host_configured_and_manifest_bounded() {
        let module = ExtensionModuleConfig {
            name: "example-extension".to_string(),
            registry: "ghcr.io/waddle-social/waddle/extensions/example-extension".to_string(),
            digest: Some(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_string(),
            ),
            tag: None,
            namespace: "urn:example:extension:1".to_string(),
            config: json!({}),
            capability_grants: vec![
                ExtensionCapability::Commands,
                ExtensionCapability::OutboundHttpRequest,
                ExtensionCapability::HostMessageSend,
            ],
            allowed_http_origins: Vec::new(),
            config_secret_files: Default::default(),
            local_path: None,
        };
        let manifest = ExtensionManifest {
            id: PluginId::new("example-extension").expect("static plugin id is valid"),
            name: DisplayText::new("Example Extension").expect("static display text is valid"),
            version: crate::types::PluginVersion::new("0.1.0")
                .expect("static plugin version is valid"),
            payloads: Vec::new(),
            capabilities: vec![
                ExtensionCapability::Commands,
                ExtensionCapability::OutboundHttpRequest,
            ],
            commands: Vec::new(),
            pubsub_nodes: Vec::new(),
            artifact: None,
        };

        let grants = runtime_grants_for_module(&module, &manifest);

        assert!(grants.contains(&ExtensionCapability::Commands));
        assert!(grants.contains(&ExtensionCapability::OutboundHttpRequest));
        assert!(!grants.contains(&ExtensionCapability::HostMessageSend));
        assert!(!grants.contains(&ExtensionCapability::HostMamRead));
    }

    #[tokio::test]
    async fn enrich_message_does_not_fallback_without_loaded_actor() {
        let manager = ExtensionManager {
            actors: Vec::new(),
            feature_namespaces: vec!["urn:example:extension:1".to_string()],
            launch_signing_key: None,
        };

        let mut msg = Message::new(None);
        msg.bodies.insert(
            String::new(),
            Body("https://github.com/waddle-social/waddle".to_string()),
        );

        assert_eq!(manager.enrich_message(&mut msg).await, 0);
        assert!(msg.payloads.is_empty());
    }

    struct TestArtifacts {
        root: PathBuf,
    }

    impl TestArtifacts {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should move forward")
                .as_nanos();
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("test-artifacts")
                .join(format!("manager-{nonce}-{}", std::process::id()));
            fs::create_dir_all(&root).expect("artifact directory should be created");
            Self { root }
        }

        fn write(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::write(&path, contents).expect("artifact file should be written");
            path
        }
    }

    impl Drop for TestArtifacts {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
