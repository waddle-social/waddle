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
use xmpp_parsers::message::Message;

use crate::actor::WasmExtensionActor;
use crate::config::{ExtensionConfig, ExtensionModuleConfig};
use crate::oci::OciExtensionPuller;
use crate::runtime::{LoadedExtension, WasmRuntime};
use crate::types::{
    is_official_namespace, message_has_framework_envelope, BotGroupchatResponsePurpose,
    CommandAction, CommandInvocation, CommandNode, CommandSessionId, DetectedLink, DisplayText,
    ExtensionEffect, ExtensionEnvelope, ExtensionEvent, ExtensionManifest, FullJidValue,
    LaunchContext, LaunchId, LaunchInvocation, LinkTarget, MessageContext, MessageHook,
    PayloadNamespace, PluginId, ReplyTarget, RoomJid, StanzaId, ThreadId, WaddleId,
    AI_CHATBOT_NAMESPACE, AI_CHATBOT_PLUGIN_ID, FRAMEWORK_NAMESPACE,
};

const MAX_DETECTED_LINKS: usize = 3;
const EXTENSION_ENRICH_TIMEOUT: Duration = Duration::from_millis(750);

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
    pub requester: WaddleId,
    pub session_id: Option<CommandSessionId>,
    pub action: Option<CommandAction>,
    pub fields: Vec<crate::types::FormFieldValue>,
    pub form: Option<crate::types::DataForm>,
    pub expires_at: Option<crate::types::Timestamp>,
    pub launch_token: &'a str,
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
            };

            let manifest = actor.manifest();
            validate_manifest_against_module(module, &manifest)?;
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

    pub fn command_nodes(&self) -> Vec<(String, String, Option<String>)> {
        self.actors
            .iter()
            .flat_map(|actor| {
                actor
                    .manifest()
                    .commands
                    .into_iter()
                    .filter(|command| command.node != CommandNode::invoke())
                    .map(|command| {
                        (
                            command.node.into_string(),
                            command.name.into_string(),
                            command.composer_prefix,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub async fn invoke_command(
        &self,
        node: &str,
        waddle_id: WaddleId,
        session_id: Option<CommandSessionId>,
        action: Option<CommandAction>,
        fields: Vec<crate::types::FormFieldValue>,
        form: Option<crate::types::DataForm>,
    ) -> Vec<ExtensionEffect> {
        let Ok(command_node) = CommandNode::new(node.to_string()) else {
            return Vec::new();
        };
        let dispatch_node = command_node.clone();
        let event = ExtensionEvent::Command(CommandInvocation {
            waddle_id,
            command_node: dispatch_node,
            session_id,
            action,
            form,
            fields,
        });
        for actor in &self.actors {
            if actor.manifest().declares_command(&command_node) {
                return self.sign_effects(actor.handle_event(event).await);
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
        if message_has_framework_envelope(msg) {
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
            let source_stanza_id = msg.id.clone().and_then(|id| StanzaId::new(id).ok());
            let room = msg
                .to
                .as_ref()
                .and_then(|jid| RoomJid::new(jid.to_bare().to_string()).ok());
            let sender = if room.is_some() {
                None
            } else {
                msg.from
                    .as_ref()
                    .and_then(|jid| FullJidValue::new(jid.to_string()).ok())
            };
            let event = ExtensionEvent::MessageHook(MessageHook {
                context: MessageContext {
                    waddle_id,
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
                if !manifest.declares_capability(crate::types::ExtensionCapability::MessageEnrich)
                    && !manifest
                        .declares_capability(crate::types::ExtensionCapability::MessageObserve)
                {
                    return None;
                }
                let actor_name = actor.manifest().id.to_string();
                let manifest = actor.manifest();
                let actor = Arc::clone(actor);
                let event = event.clone();
                Some(async move {
                    match timeout(EXTENSION_ENRICH_TIMEOUT, actor.handle_event(event)).await {
                        Ok(effects) => (actor_name, manifest, effects),
                        Err(_) => {
                            warn!(
                                extension = %actor_name,
                                timeout_secs = EXTENSION_ENRICH_TIMEOUT.as_secs(),
                                "extension enrichment timed out; continuing fail-open"
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
                        ExtensionEffect::BotGroupchatResponse(mut response) => {
                            response.purpose =
                                bot_groupchat_response_purpose_for_manifest(&manifest);
                            emitted_effects.push(ExtensionEffect::BotGroupchatResponse(response));
                        }
                        ExtensionEffect::PublishPubSub(_)
                        | ExtensionEffect::ReferenceArtifact(_)
                        | ExtensionEffect::HostWarning(_)
                        | ExtensionEffect::Noop => {}
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
                | ExtensionEffect::BotGroupchatResponse(_)
                | ExtensionEffect::ReferenceArtifact(_)
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

fn bot_groupchat_response_purpose_for_manifest(
    manifest: &ExtensionManifest,
) -> BotGroupchatResponsePurpose {
    let is_ai_chatbot = manifest.id.as_str() == AI_CHATBOT_PLUGIN_ID
        && manifest
            .payloads
            .iter()
            .any(|rule| rule.root.namespace.as_str() == AI_CHATBOT_NAMESPACE);
    if is_ai_chatbot {
        BotGroupchatResponsePurpose::AiProviderFallback
    } else {
        BotGroupchatResponsePurpose::Message
    }
}

fn validate_manifest_against_module(
    module: &ExtensionModuleConfig,
    manifest: &crate::types::ExtensionManifest,
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
    Ok(())
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
        bot_groupchat_response_purpose_for_manifest, detect_links, effective_module_config_json,
        effective_module_config_with_reader, EffectiveModuleConfigError, ExtensionManager,
        MAX_DETECTED_LINKS,
    };
    use crate::config::{ExtensionConfig, ExtensionModuleConfig};
    use crate::types::{
        BotGroupchatResponsePurpose, DisplayText, ExtensionCapability, ExtensionManifest,
        PayloadNamespace, PayloadRoot, PayloadRule, PayloadSurface, PluginId, PluginVersion,
        AI_CHATBOT_NAMESPACE, AI_CHATBOT_PLUGIN_ID,
    };

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
    fn ai_provider_fallback_purpose_is_scoped_to_ai_chatbot_manifest() {
        let ai_chatbot = bot_manifest(AI_CHATBOT_PLUGIN_ID, AI_CHATBOT_NAMESPACE);
        assert_eq!(
            bot_groupchat_response_purpose_for_manifest(&ai_chatbot),
            BotGroupchatResponsePurpose::AiProviderFallback
        );

        let wrong_id = bot_manifest("other-chatbot", AI_CHATBOT_NAMESPACE);
        assert_eq!(
            bot_groupchat_response_purpose_for_manifest(&wrong_id),
            BotGroupchatResponsePurpose::Message
        );

        let wrong_namespace = bot_manifest(AI_CHATBOT_PLUGIN_ID, "urn:waddle:other-chatbot:1");
        assert_eq!(
            bot_groupchat_response_purpose_for_manifest(&wrong_namespace),
            BotGroupchatResponsePurpose::Message
        );
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

    fn bot_manifest(id: &str, namespace: &str) -> ExtensionManifest {
        ExtensionManifest {
            id: PluginId::new(id.to_string()).expect("plugin id"),
            name: DisplayText::new("test bot").expect("name"),
            version: PluginVersion::new("0.1.0").expect("version"),
            payloads: vec![PayloadRule {
                surface: PayloadSurface::MessageEnrichment,
                root: PayloadRoot::new(
                    PayloadNamespace::new(namespace.to_string()).expect("namespace"),
                    "assistant-answer",
                )
                .expect("payload root"),
            }],
            capabilities: vec![
                ExtensionCapability::MessageObserve,
                ExtensionCapability::BotGroupchatSend,
            ],
            commands: Vec::new(),
            pubsub_nodes: Vec::new(),
            artifact: None,
        }
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
