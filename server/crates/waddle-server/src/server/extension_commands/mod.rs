mod forms;
pub(crate) mod pubsub;

use crate::server::AppState;
use forms::{
    extension_command_action, extension_command_fields, extension_data_form, extension_session_id,
};
use pubsub::{ExtensionPubSubContext, extension_command_result};
use std::sync::Arc;
use waddle_extensions::{
    ExtensionConfig, ExtensionManager, FullJidValue, INVOKE_COMMAND_NODE, LaunchContext, LaunchId,
    RoomJid as ExtensionRoomJid, StanzaId, WaddleId, host_tools as ext_host,
};
use waddle_xmpp::pubsub::PubSubStorage;

pub(crate) async fn register_extension_commands(
    extension_manager: Arc<ExtensionManager>,
    command_registry: Arc<waddle_xmpp::commands::CommandRegistry>,
    pubsub_storage: Arc<dyn PubSubStorage>,
    extension_pubsub_owner: jid::BareJid,
    app_state: Arc<AppState>,
) {
    let launch_manager = Arc::clone(&extension_manager);
    let launch_storage = Arc::clone(&pubsub_storage);
    let launch_owner = extension_pubsub_owner.clone();
    let launch_app_state = Arc::clone(&app_state);
    command_registry
        .register(INVOKE_COMMAND_NODE, "Invoke extension action", move |ctx| {
            let manager = Arc::clone(&launch_manager);
            let storage = Arc::clone(&launch_storage);
            let owner = launch_owner.clone();
            let app_state = Arc::clone(&launch_app_state);
            async move {
                let submitted_form = ctx.command.form.as_ref();
                let fields = extension_command_fields(submitted_form);
                let Some(plugin) = extension_field_value(&fields, "plugin")
                    .or_else(|| extension_field_value(&fields, "waddle#plugin_id"))
                else {
                    return extension_warning_result("Extension launch is missing plugin");
                };
                let Some(launch) = extension_field_value(&fields, "launch-id")
                    .or_else(|| extension_field_value(&fields, "waddle#launch_id"))
                else {
                    return extension_warning_result("Extension launch is missing launch-id");
                };
                let Some(action_id) = extension_field_value(&fields, "action")
                    .or_else(|| extension_field_value(&fields, "waddle#action_id"))
                else {
                    return extension_warning_result("Extension launch is missing action id");
                };
                let Some(launch_token) = extension_field_value(&fields, "launch-token")
                    .or_else(|| extension_field_value(&fields, "waddle#launch_token"))
                else {
                    return extension_warning_result("Extension launch is missing launch token");
                };
                let waddle_id = extension_field_value(&fields, "waddle-id")
                    .or_else(|| extension_field_value(&fields, "waddle#waddle_id"))
                    .unwrap_or_else(|| ctx.from.to_string());
                let Ok(launch_id) = LaunchId::new(launch) else {
                    return extension_warning_result("Extension launch id is invalid");
                };
                let Ok(waddle_id) = WaddleId::new(waddle_id) else {
                    return extension_warning_result("Extension launch waddle id is invalid");
                };
                let room = extension_field_value(&fields, "room")
                    .or_else(|| extension_field_value(&fields, "waddle#room_jid"))
                    .and_then(|value| ExtensionRoomJid::new(value).ok());
                let source_stanza_id = extension_field_value(&fields, "source-stanza-id")
                    .or_else(|| extension_field_value(&fields, "waddle#message_stanza_id"))
                    .and_then(|value| StanzaId::new(value).ok());
                let expires_at = extension_field_value(&fields, "expires-at")
                    .or_else(|| extension_field_value(&fields, "waddle#expires_at"))
                    .and_then(|value| waddle_extensions::Timestamp::new(value).ok());
                let context = LaunchContext {
                    waddle_id,
                    room,
                    source_stanza_id,
                };
                if !manager.validates_launch_invocation(
                    waddle_extensions::manager::LaunchValidationRequest {
                        plugin_name: &plugin,
                        action_id: &action_id,
                        launch_id: &launch_id,
                        context: &context,
                        fields: &fields,
                        expires_at: expires_at.as_ref(),
                        launch_token: &launch_token,
                    },
                ) {
                    return extension_warning_result(
                        "Extension launch token is missing, expired, or invalid",
                    );
                }
                let effects = manager
                    .invoke_launch(waddle_extensions::manager::LaunchInvocationRequest {
                        plugin_name: &plugin,
                        action_id: &action_id,
                        launch_id,
                        context,
                        requester: FullJidValue::new(ctx.from.to_string())
                            .expect("requester JID string is non-empty"),
                        session_id: extension_session_id(ctx.command.session_id),
                        action: ctx.command.action.map(extension_command_action),
                        fields,
                        form: submitted_form.and_then(extension_data_form),
                        expires_at,
                        launch_token: &launch_token,
                    })
                    .await;
                extension_command_result(
                    effects,
                    Some(ExtensionPubSubContext {
                        storage,
                        owner,
                        app_state,
                        extension_manager: manager,
                        authenticated_user_id: ctx.authenticated_user_id,
                    }),
                )
                .await
            }
        })
        .await;

    for (node, name) in extension_manager.command_nodes() {
        let manager = Arc::clone(&extension_manager);
        let storage = Arc::clone(&pubsub_storage);
        let owner = extension_pubsub_owner.clone();
        let app_state = Arc::clone(&app_state);
        let registered_node = node.clone();
        command_registry
            .register(node, name, move |ctx| {
                let manager = Arc::clone(&manager);
                let storage = Arc::clone(&storage);
                let owner = owner.clone();
                let app_state = Arc::clone(&app_state);
                let registered_node = registered_node.clone();
                async move {
                    let waddle_id = match WaddleId::new(ctx.from.to_string()) {
                        Ok(value) => value,
                        Err(error) => {
                            return waddle_xmpp::commands::CommandResult::Completed {
                                form: None,
                                notes: vec![waddle_xmpp::commands::Note::warn(format!(
                                    "Invalid requester JID: {error}"
                                ))],
                            };
                        }
                    };
                    let submitted_form = ctx.command.form.as_ref();
                    let fields = extension_command_fields(submitted_form);
                    let room = extension_field_value(&fields, "room")
                        .or_else(|| extension_field_value(&fields, "waddle#room_jid"))
                        .and_then(|value| ExtensionRoomJid::new(value).ok());
                    let effects = manager
                        .invoke_command(waddle_extensions::manager::CommandInvocationRequest {
                            node: &registered_node,
                            waddle_id,
                            room,
                            requester: match waddle_extensions::FullJidValue::new(
                                ctx.from.to_string(),
                            ) {
                                Ok(value) => value,
                                Err(error) => {
                                    return waddle_xmpp::commands::CommandResult::Completed {
                                        form: None,
                                        notes: vec![waddle_xmpp::commands::Note::warn(format!(
                                            "Invalid requester JID: {error}"
                                        ))],
                                    };
                                }
                            },
                            session_id: extension_session_id(ctx.command.session_id),
                            action: ctx.command.action.map(extension_command_action),
                            fields,
                            form: submitted_form.and_then(extension_data_form),
                        })
                        .await;
                    extension_command_result(
                        effects,
                        Some(ExtensionPubSubContext {
                            storage,
                            owner,
                            app_state,
                            extension_manager: manager,
                            authenticated_user_id: ctx.authenticated_user_id,
                        }),
                    )
                    .await
                }
            })
            .await;
    }
}

pub(crate) async fn build_extension_manager(
    server_config: &crate::config::ServerConfig,
    xmpp_domain: &str,
    deferred_extension_host_tools: Arc<
        crate::server::extension_host_tools::DeferredExtensionHostTools,
    >,
) -> anyhow::Result<Arc<ExtensionManager>> {
    let extension_launch_key = server_config
        .session_key
        .clone()
        .unwrap_or_else(|| format!("development-extension-launch-key:{xmpp_domain}"));

    let manager = match ExtensionManager::from_config_with_host_tools(
        server_config.extensions.clone(),
        Arc::clone(&deferred_extension_host_tools) as Arc<dyn ext_host::ExtensionHostTools>,
    )
    .await
    {
        Ok(mgr) => mgr.with_launch_signing_key(extension_launch_key.as_bytes()),
        Err(error) => {
            if server_config.extensions.enabled && !server_config.extensions.modules.is_empty() {
                return Err(anyhow::anyhow!(
                    "failed to initialize configured extensions: {error}"
                ));
            }
            tracing::warn!(error = %error, "Failed to initialize disabled extension manager; continuing without extensions");
            ExtensionManager::from_config_with_host_tools(
                ExtensionConfig {
                    enabled: false,
                    cache_dir: String::new(),
                    modules: Vec::new(),
                },
                Arc::clone(&deferred_extension_host_tools) as Arc<dyn ext_host::ExtensionHostTools>,
            )
            .await
            .map(|mgr| mgr.with_launch_signing_key(extension_launch_key.as_bytes()))
            .expect("BUG: failed to create disabled ExtensionManager")
        }
    };

    Ok(Arc::new(manager))
}

fn extension_field_value(
    fields: &[waddle_extensions::FormFieldValue],
    name: &str,
) -> Option<String> {
    fields
        .iter()
        .find(|field| field.name.as_str() == name)
        .and_then(|field| field.values.first())
        .map(|value| value.as_str().to_string())
}

fn extension_warning_result(message: &str) -> waddle_xmpp::commands::CommandResult {
    waddle_xmpp::commands::CommandResult::Completed {
        form: None,
        notes: vec![waddle_xmpp::commands::Note::warn(message.to_string())],
    }
}
