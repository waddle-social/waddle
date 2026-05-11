use super::*;

impl ExtensionManager {
    pub async fn invoke_command(
        &self,
        request: CommandInvocationRequest<'_>,
    ) -> Vec<ExtensionEffect> {
        let CommandInvocationRequest {
            node,
            waddle_id,
            room,
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
            room,
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
        if !self.verify_launch_token(LaunchTokenVerification {
            plugin: &plugin_id,
            action: &action_id,
            launch_id: &launch_id,
            context: &context,
            fields: &fields,
            expires_at: expires_at.as_ref(),
            token: launch_token,
        }) {
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
            if actor.has_grant(ExtensionCapability::Launch)
                && actor.manifest().id.as_str() == plugin_name
            {
                return self.sign_effects(actor.handle_event(event).await);
            }
        }
        Vec::new()
    }

    pub async fn invoke_provider_webhook(
        &self,
        plugin_name: &str,
        event: ProviderWebhook,
    ) -> Vec<ExtensionEffect> {
        let Ok(plugin_id) = PluginId::new(plugin_name.to_string()) else {
            return Vec::new();
        };
        let waddle_id = event.waddle_id.clone();
        let event = ExtensionEvent::ProviderWebhook(event);
        for actor in &self.actors {
            if actor.manifest().id == plugin_id {
                return match timeout(
                    EXTENSION_PROVIDER_WEBHOOK_TIMEOUT,
                    actor.handle_event_for_waddle(event, waddle_id),
                )
                .await
                {
                    Ok(effects) => self.sign_effects(effects),
                    Err(_) => vec![ExtensionEffect::HostWarning(
                        DisplayText::new(format!(
                            "Extension provider webhook {plugin_name} timed out"
                        ))
                        .expect("timeout warning is non-empty"),
                    )],
                };
            }
        }
        Vec::new()
    }

    pub fn validates_launch_invocation(&self, request: LaunchValidationRequest<'_>) -> bool {
        let Ok(plugin_id) = PluginId::new(request.plugin_name.to_string()) else {
            return false;
        };
        let Ok(action_id) = crate::types::ActionId::new(request.action_id.to_string()) else {
            return false;
        };
        self.verify_launch_token(LaunchTokenVerification {
            plugin: &plugin_id,
            action: &action_id,
            launch_id: request.launch_id,
            context: request.context,
            fields: request.fields,
            expires_at: request.expires_at,
            token: request.launch_token,
        })
    }
}
