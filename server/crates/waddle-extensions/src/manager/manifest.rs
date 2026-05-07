use super::*;

pub(super) fn validate_manifest_against_module(
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
    if !manifest.routes.is_empty()
        && !manifest.declares_capability(ExtensionCapability::UiDeclarative)
    {
        bail!(
            "extension {} declared UI routes without ui.declarative capability",
            module.name
        );
    }
    let mut route_ids = HashSet::new();
    for route in &manifest.routes {
        if route.plugin != manifest.id {
            bail!(
                "extension {} declared route {} for plugin {}; route plugin must match manifest id {}",
                module.name,
                route.id,
                route.plugin,
                manifest.id
            );
        }
        if !route_ids.insert(route.id.clone()) {
            bail!(
                "extension {} declared duplicate route id {}",
                module.name,
                route.id
            );
        }
        if route.payload_namespace != expected_namespace {
            bail!(
                "extension {} declared route {} payload namespace {}; expected configured namespace {}",
                module.name,
                route.id,
                route.payload_namespace,
                expected_namespace
            );
        }
        if !manifest.declares_pubsub_node(&route.state_node) {
            bail!(
                "extension {} declared route {} state node {} without a matching PubSub node declaration",
                module.name,
                route.id,
                route.state_node
            );
        }
    }
    for capability in &manifest.capabilities {
        if !module.capability_grants.contains(capability) {
            bail!(
                "extension {} requires explicit operator grant for declared capability {}",
                module.name,
                capability.as_str()
            );
        }
    }
    validate_outbound_http_origins(module, manifest)?;
    Ok(())
}

pub(super) fn runtime_grants_for_module(
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

pub(super) fn validate_outbound_http_origins(
    module: &ExtensionModuleConfig,
    manifest: &ExtensionManifest,
) -> Result<()> {
    let declares_outbound_http =
        manifest.declares_capability(ExtensionCapability::OutboundHttpRequest);
    if !declares_outbound_http {
        if !module.allowed_http_origins.is_empty() {
            bail!(
                "extension {} configured allowedHttpOrigins without declaring the outbound-http-request capability",
                module.name
            );
        }
        return Ok(());
    }
    for origin in &module.allowed_http_origins {
        let url = reqwest::Url::parse(origin).map_err(|error| {
            anyhow::anyhow!(
                "extension {} allowedHttpOrigins entry {origin} is not a valid URL: {error}",
                module.name
            )
        })?;
        if url.scheme() != "https" {
            bail!(
                "extension {} allowedHttpOrigins entry {origin} must use HTTPS",
                module.name
            );
        }
        if url.host_str().is_none() {
            bail!(
                "extension {} allowedHttpOrigins entry {origin} must include a host",
                module.name
            );
        }
    }
    Ok(())
}
