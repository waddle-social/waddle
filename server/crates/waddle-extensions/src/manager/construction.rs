//! Extension manager construction and configured runtime initialization.

use super::{
    bail, effective_module_config_json, push_feature_namespace, remove_invalid_cached_extension,
    runtime_grants_for_module, validate_manifest_against_module, Arc, BareJid, CommandNode,
    DenyingExtensionHostTools, DenyingExtensionInferenceHost, Digest, ExtensionCapability,
    ExtensionConfig, ExtensionHostTools, ExtensionInferenceHost, ExtensionManager,
    ExtensionManifest, ExtensionModuleConfig, HashMap, HashSet, LoadedExtension,
    OciExtensionPuller, Path, PayloadNamespace, PluginId, Result, Sha256, WasmExtensionActor,
    WasmRuntime,
};
use crate::types::CommandDescriptor;
use crate::types::ConfiguredRoomObserver;
use crate::types::ExtensionRouteDescriptor;
use crate::types::Sha256Digest;

impl ExtensionManager {
    /// Build an `ExtensionManager` from the given configuration.
    ///
    /// Configured extension modules fail fast. Message enrichment itself remains
    /// fail-open so user messages are not lost after startup.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, artifact loading/verification
    /// failures, Wasm initialization failures, invalid manifests/routes, or duplicate
    /// extension identifiers.
    pub async fn from_config(config: ExtensionConfig) -> Result<Self> {
        Self::from_config_with_host_tools(config, Arc::new(DenyingExtensionHostTools)).await
    }

    /// Load configured components using the supplied host services, rejecting conflicting
    /// plugin IDs, command nodes, and payload namespace ownership before serving
    /// invocations.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, artifact loading/verification
    /// failures, Wasm initialization failures, invalid manifests/routes, or duplicate
    /// extension identifiers.
    pub async fn from_config_with_host_tools(
        config: ExtensionConfig,
        host_tools: Arc<dyn ExtensionHostTools>,
    ) -> Result<Self> {
        Self::from_config_with_hosts(
            config,
            host_tools,
            Arc::new(DenyingExtensionInferenceHost),
        )
        .await
    }

    /// Load configured components using both host tools and an explicit
    /// brokered [`ExtensionInferenceHost`].
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, artifact loading/verification
    /// failures, Wasm initialization failures, invalid manifests/routes, or duplicate
    /// extension identifiers.
    pub async fn from_config_with_hosts(
        config: ExtensionConfig,
        host_tools: Arc<dyn ExtensionHostTools>,
        inference_host: Arc<dyn ExtensionInferenceHost>,
    ) -> Result<Self> {
        if !config.enabled {
            return Ok(Self {
                actors: Vec::new(),
                feature_namespaces: Vec::new(),
                route_descriptors: Vec::new(),
                launch_signing_key: None,
                tenant_id: None,
                resource_controller: None,
                inference_host,
            });
        }
        config.validate().map_err(anyhow::Error::msg)?;

        let runtime = WasmRuntime::try_new()?;
        let puller = OciExtensionPuller::new(&config.cache_dir);
        let mut actors = Vec::new();
        let mut feature_namespaces = Vec::new();
        let mut route_descriptors = Vec::new();
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
                Ok(loaded) => loaded.with_limits(module.runtime_limits.clone())?,
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
            .with_host_tools(Arc::clone(&host_tools))
            .with_inference_host(Arc::clone(&inference_host));

            let manifest = actor.manifest();
            validate_manifest_against_module(module, &manifest)?;
            let observer = configured_room_observer(module, &manifest, &wasm_path, &config_json)?;
            let actor = actor
                .with_room_observer(observer)
                .with_grants(runtime_grants_for_module(module, &manifest))
                .with_allowed_http_origins(module.allowed_http_origins.clone())
                .with_provider_room_grants(parse_provider_room_grants(module)?);
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
            if manifest.declares_capability(ExtensionCapability::UiDeclarative)
                && actor.has_grant(ExtensionCapability::UiDeclarative)
            {
                route_descriptors.extend(manifest.routes.iter().cloned());
            }

            actors.push(Arc::new(actor));
        }

        Ok(Self {
            actors,
            feature_namespaces,
            route_descriptors,
            launch_signing_key: None,
            tenant_id: None,
            resource_controller: None,
            inference_host,
        })
    }

    /// Install or replace the brokered [`ExtensionInferenceHost`] across all actors.
    #[must_use]
    pub fn with_inference_host(mut self, inference_host: Arc<dyn ExtensionInferenceHost>) -> Self {
        self.inference_host = Arc::clone(&inference_host);
        self.actors = self
            .actors
            .into_iter()
            .map(|actor| match Arc::try_unwrap(actor) {
                Ok(inner) => Arc::new(inner.with_inference_host(Arc::clone(&inference_host))),
                Err(shared) => shared,
            })
            .collect();
        self
    }

    /// Load configured components bound to a specific canonical [`waddle_broker_core::identity::TenantId`]
    /// and per-tenant [`waddle_broker_core::quotas::TenantResourceController`].
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, artifact loading/verification
    /// failures, Wasm initialization failures, invalid manifests/routes, or duplicate
    /// extension identifiers.
    pub async fn from_config_for_tenant(
        tenant_id: waddle_broker_core::identity::TenantId,
        config: ExtensionConfig,
        host_tools: Arc<dyn ExtensionHostTools>,
        resource_controller: Arc<waddle_broker_core::quotas::TenantResourceController>,
    ) -> Result<Self> {
        Self::from_config_for_tenant_with_hosts(
            tenant_id,
            config,
            host_tools,
            Arc::new(DenyingExtensionInferenceHost),
            resource_controller,
        )
        .await
    }

    /// Load configured components bound to a canonical [`waddle_broker_core::identity::TenantId`],
    /// explicit host tools, brokered [`ExtensionInferenceHost`], and per-tenant
    /// [`waddle_broker_core::quotas::TenantResourceController`].
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, artifact loading/verification
    /// failures, Wasm initialization failures, invalid manifests/routes, or duplicate
    /// extension identifiers.
    pub async fn from_config_for_tenant_with_hosts(
        tenant_id: waddle_broker_core::identity::TenantId,
        config: ExtensionConfig,
        host_tools: Arc<dyn ExtensionHostTools>,
        inference_host: Arc<dyn ExtensionInferenceHost>,
        resource_controller: Arc<waddle_broker_core::quotas::TenantResourceController>,
    ) -> Result<Self> {
        let mut mgr = Self::from_config_with_hosts(config, host_tools, inference_host).await?;
        mgr.tenant_id = Some(tenant_id.clone());
        mgr.resource_controller = Some(Arc::clone(&resource_controller));
        mgr.actors = mgr
            .actors
            .into_iter()
            .map(|actor| match Arc::try_unwrap(actor) {
                Ok(inner) => Arc::new(
                    inner
                        .with_tenant_id(tenant_id.clone())
                        .with_resource_controller(Arc::clone(&resource_controller)),
                ),
                Err(shared) => shared,
            })
            .collect();
        Ok(mgr)
    }

    /// Return the canonical [`waddle_broker_core::identity::TenantId`] bound to this manager, if any.
    #[must_use]
    pub fn tenant_id(&self) -> Option<&waddle_broker_core::identity::TenantId> {
        self.tenant_id.as_ref()
    }

    /// Install a nonempty launch-signing key; an empty key leaves the existing key unchanged.
    #[must_use]
    pub fn with_launch_signing_key(mut self, key: impl AsRef<[u8]>) -> Self {
        let key = key.as_ref();
        if !key.is_empty() {
            self.launch_signing_key = Some(key.to_vec());
        }
        self
    }

    /// Load `WADDLE_EXTENSIONS_JSON`, then resolve and initialize every configured component.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, artifact loading/verification
    /// failures, Wasm initialization failures, invalid manifests/routes, or duplicate
    /// extension identifiers.
    pub async fn from_env() -> Result<Self> {
        let config = ExtensionConfig::from_env().map_err(anyhow::Error::msg)?;
        Self::from_config(config).await
    }

    /// Complete loaded plugin set and its already parsed provider room grants.
    /// Manifests have passed explicit operator capability-grant validation.
    pub fn configured_plugins(
        &self,
    ) -> impl Iterator<Item = (ExtensionManifest, Vec<BareJid>)> + '_ {
        self.actors
            .iter()
            .map(|actor| (actor.manifest(), actor.provider_room_grants().to_vec()))
    }

    /// Borrow the distinct extension payload namespaces selected for feature advertisement.
    #[must_use]
    pub fn feature_namespaces(&self) -> &[String] {
        &self.feature_namespaces
    }

    /// Clone the extension payload namespaces selected for feature advertisement.
    #[must_use]
    pub fn extension_features(&self) -> Vec<String> {
        self.feature_namespaces.clone()
    }

    /// Return command node/name pairs for command-granted plugins, excluding the generic
    /// framework invoke node.
    #[must_use]
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

    /// Return plugin identities and command descriptors for command-granted plugins,
    /// excluding the generic invoke node.
    #[must_use]
    pub fn command_descriptors(&self) -> Vec<(crate::types::PluginId, CommandDescriptor)> {
        self.actors
            .iter()
            .filter(|actor| actor.has_grant(ExtensionCapability::Commands))
            .flat_map(|actor| {
                let manifest = actor.manifest();
                let plugin = manifest.id.clone();
                manifest
                    .commands
                    .into_iter()
                    .filter(|command| command.node != CommandNode::invoke())
                    .map(move |command| (plugin.clone(), command))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Borrow the extension routes retained for discovery and UI rendering.
    #[must_use]
    pub fn route_descriptors(&self) -> &[ExtensionRouteDescriptor] {
        &self.route_descriptors
    }

    /// Whether a valid plugin ID identifies a currently loaded extension.
    #[must_use]
    pub fn has_plugin(&self, plugin_name: &str) -> bool {
        let Ok(plugin_id) = PluginId::try_new(plugin_name.to_string()) else {
            return false;
        };
        self.actors
            .iter()
            .any(|actor| actor.manifest().id == plugin_id)
    }

    /// Returns a clone of the manifest for the named plugin if it is
    /// loaded. Used by host tools that need to know an extension's
    /// declared payload namespace at runtime (e.g. to gate PubSub reads
    /// to the extension's own namespace).
    #[must_use]
    pub fn manifest_for_plugin(
        &self,
        plugin_name: &str,
    ) -> Option<crate::types::ExtensionManifest> {
        let plugin_id = PluginId::try_new(plugin_name.to_string()).ok()?;
        self.actors
            .iter()
            .find(|actor| actor.manifest().id == plugin_id)
            .map(|actor| actor.manifest())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ExtensionManager {
    /// Declare `avatar` on a loaded plugin's manifest profile, as an
    /// operator-installed manifest would.
    ///
    /// # Panics
    ///
    /// Panics if the plugin is not loaded or its actor has already been shared through
    /// another `Arc`.
    #[must_use]
    pub fn with_profile_avatar(
        mut self,
        plugin: &PluginId,
        avatar: crate::types::ArtifactReference,
    ) -> Self {
        self.actors
            .iter_mut()
            .find(|actor| actor.manifest().id == *plugin)
            .and_then(Arc::get_mut)
            .expect("the plugin is loaded and its actor not yet shared")
            .set_profile_avatar(avatar);
        self
    }
}

fn parse_provider_room_grants(module: &ExtensionModuleConfig) -> Result<Vec<BareJid>> {
    module
        .provider_room_grants
        .iter()
        .map(|room| {
            room.parse::<BareJid>().map_err(|error| {
                anyhow::anyhow!(
                    "extension {} providerRoomGrants entry {room:?} is not a valid bare room JID: {error}",
                    module.name
                )
            })
        })
        .collect()
}

fn configured_room_observer(
    module: &ExtensionModuleConfig,
    manifest: &ExtensionManifest,
    wasm_path: &Path,
    effective_config: &str,
) -> Result<Option<ConfiguredRoomObserver>> {
    let Some(observation) = &module.room_observation else {
        return Ok(None);
    };
    let grants = runtime_grants_for_module(module, manifest);
    if !grants.contains(&ExtensionCapability::MessageObserve)
        || !manifest.declares_capability(ExtensionCapability::MessageObserve)
    {
        bail!(
            "extension {} observation configuration requires message.observe",
            manifest.id
        );
    }
    let artifact = Sha256::digest(std::fs::read(wasm_path)?);
    // Frozen identity includes effective secrets without persisting or logging their values.
    let identity_input = serde_json::to_vec(&(
        hex::encode(artifact),
        effective_config,
        &module.capability_grants,
        &module.allowed_http_origins,
        observation,
        &module.runtime_limits,
    ))?;
    Ok(Some(ConfiguredRoomObserver {
        plugin: manifest.id.clone(),
        generation: observation.generation,
        identity: Sha256Digest::try_new(hex::encode(Sha256::digest(identity_input)))?,
        scope: observation.scope.clone(),
        max_concurrent: observation.max_concurrent,
    }))
}
