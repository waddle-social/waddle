use super::*;

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
                route_descriptors: Vec::new(),
                launch_signing_key: None,
            });
        }
        config.validate().map_err(anyhow::Error::msg)?;

        let runtime = WasmRuntime::new()?;
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

    pub fn command_descriptors(
        &self,
    ) -> Vec<(crate::types::PluginId, crate::types::CommandDescriptor)> {
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

    pub fn route_descriptors(&self) -> &[crate::types::ExtensionRouteDescriptor] {
        &self.route_descriptors
    }
}
