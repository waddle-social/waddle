use crate::permissions::{CheckPermission, Object, ObjectType, Permission, Subject};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use crate::server::extension_commands::forms::{
    extension_data_form_to_xmpp, extension_enrichment_result_form, extension_enrichment_texts,
};
use crate::server::managed_channel_policy::{
    server_policy_for_managed_channel, ManagedChannelServerPolicy,
    DEPLOYMENT_MEMBERSHIP_PERMISSIONS,
};
use crate::server::AppState;
use std::sync::Arc;
use waddle_extensions::{ExtensionEffect, ExtensionManager, PubSubPublish};
use waddle_xmpp::pubsub::{NodeConfig, PubSubItem, PubSubStorage};

pub(crate) struct ExtensionPubSubContext {
    pub(crate) storage: Arc<dyn PubSubStorage>,
    pub(crate) owner: jid::BareJid,
    pub(crate) app_state: Arc<AppState>,
    pub(crate) extension_manager: Arc<ExtensionManager>,
    pub(crate) plugin_id: waddle_extensions::PluginId,
    pub(crate) authenticated_user_id: Option<String>,
}

async fn authorize_extension_pubsub_publish(
    context: &ExtensionPubSubContext,
    node: &waddle_extensions::types::PubSubNode,
) -> Result<(), String> {
    let Some(user_id) = context.authenticated_user_id.as_deref() else {
        return Err("authenticated user required".to_string());
    };
    let manifest = context
        .extension_manager
        .manifest_for_plugin(context.plugin_id.as_str())
        .ok_or_else(|| format!("extension {} is not loaded", context.plugin_id))?;
    if !manifest.declares_pubsub_node(node) {
        return Err(format!(
            "PubSub node is not declared by extension {}",
            context.plugin_id
        ));
    }
    let Some(room) = context
        .extension_manager
        .room_for_plugin_pubsub_node(&context.plugin_id, node)
    else {
        if deployment_owner_allowed(&context.app_state, user_id).await? {
            return Ok(());
        }
        return Err("requester cannot write extension-owned PubSub state".to_string());
    };
    let room_jid: jid::BareJid = room
        .as_str()
        .parse()
        .map_err(|error| format!("invalid channel JID in PubSub node: {error}"))?;
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) else {
        return Err("PubSub node is not bound to a managed channel".to_string());
    };
    let object = Object::new(ObjectType::Channel, channel_id.clone());
    let subject = Subject::user(user_id);
    let outcast = context
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: subject.clone(),
            permission: Permission::Custom("outcast".into()),
            object: object.clone(),
        })
        .await
        .map_err(|error| format!("permission check failed: {error}"))?;
    if outcast.allowed {
        return Err("requester is not allowed in this channel".to_string());
    }
    if managed_channel_permission_allowed(
        &context.app_state,
        &subject,
        channel_id.as_str(),
        Permission::SendMessage,
    )
    .await?
    {
        Ok(())
    } else {
        Err("requester cannot write extension state for this channel".to_string())
    }
}

async fn deployment_owner_allowed(app_state: &AppState, user_id: &str) -> Result<bool, String> {
    app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(user_id),
            permission: Permission::Owner,
            object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
        })
        .await
        .map(|result| result.allowed)
        .map_err(|error| format!("permission check failed: {error}"))
}

pub(crate) async fn managed_channel_permission_allowed(
    app_state: &AppState,
    subject: &Subject,
    channel_id: &str,
    permission: Permission,
) -> Result<bool, String> {
    let policy = server_policy_for_managed_channel(channel_id, &permission);
    if policy == ManagedChannelServerPolicy::DeploymentOwnerOnly {
        let server_owner = app_state
            .permission_actor
            .ask(CheckPermission {
                subject: subject.clone(),
                permission: Permission::Owner,
                object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
            })
            .await
            .map_err(|error| format!("permission check failed: {error}"))?;
        return Ok(server_owner.allowed);
    }

    let allowed = app_state
        .permission_actor
        .ask(CheckPermission {
            subject: subject.clone(),
            permission: permission.clone(),
            object: Object::new(ObjectType::Channel, channel_id),
        })
        .await
        .map_err(|error| format!("permission check failed: {error}"))?;
    if allowed.allowed {
        return Ok(true);
    }

    if policy == ManagedChannelServerPolicy::DeploymentMembership {
        // Keep these as explicit relation/permission checks. The local permission
        // schema makes `member` inherit owner/admin, but the SpiceDB schema uses
        // server relations directly for compatibility.
        for server_permission in DEPLOYMENT_MEMBERSHIP_PERMISSIONS {
            let server_allowed = app_state
                .permission_actor
                .ask(CheckPermission {
                    subject: subject.clone(),
                    permission: server_permission,
                    object: Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                })
                .await
                .map_err(|error| format!("permission check failed: {error}"))?;
            if server_allowed.allowed {
                return Ok(true);
            }
        }
        return Ok(false);
    }

    Ok(false)
}

pub(crate) async fn extension_command_result(
    effects: Vec<ExtensionEffect>,
    pubsub: Option<ExtensionPubSubContext>,
    command_session_id: Option<String>,
) -> waddle_xmpp::commands::CommandResult {
    let mut notes = Vec::new();
    let mut result_form = None;
    for effect in effects {
        match effect {
            ExtensionEffect::PublishPubSub(publish) => match pubsub.as_ref() {
                Some(context) => {
                    match authorize_extension_pubsub_publish(context, &publish.node).await {
                        Ok(()) => {}
                        Err(error) => {
                            notes.push(waddle_xmpp::commands::Note::error(format!(
                                "PubSub publish denied: {error}"
                            )));
                            continue;
                        }
                    }
                    match publish_extension_pubsub(
                        context.storage.as_ref(),
                        &context.owner,
                        publish,
                    )
                    .await
                    {
                        Ok(item_id) => notes.push(waddle_xmpp::commands::Note::info(format!(
                            "Published PubSub item {item_id}"
                        ))),
                        Err(error) => notes.push(waddle_xmpp::commands::Note::error(format!(
                            "PubSub publish failed: {error}"
                        ))),
                    }
                }
                None => notes.push(waddle_xmpp::commands::Note::error(
                    "PubSub publish unavailable".to_string(),
                )),
            },
            ExtensionEffect::ReferenceArtifact(artifact) => {
                let text = format!("Referenced artifact {}", artifact.uri.as_str());
                notes.push(waddle_xmpp::commands::Note::info(text));
            }
            ExtensionEffect::CommandForm(form) => {
                return waddle_xmpp::commands::CommandResult::Executing {
                    form: extension_data_form_to_xmpp(form),
                    session_id: command_session_id.unwrap_or_default(),
                    notes,
                };
            }
            ExtensionEffect::HostWarning(message) => {
                notes.push(waddle_xmpp::commands::Note::error(
                    message.as_str().to_string(),
                ));
            }
            ExtensionEffect::EnrichMessage(envelope) => {
                let count = envelope.enrichments.len();
                if result_form.is_none() {
                    result_form = Some(extension_enrichment_result_form(&envelope));
                }
                let summaries = extension_enrichment_texts(&envelope);
                if summaries.is_empty() {
                    notes.push(waddle_xmpp::commands::Note::info(format!(
                        "Produced {count} message enrichment{}",
                        if count == 1 { "" } else { "s" }
                    )));
                } else {
                    notes.extend(summaries.into_iter().map(waddle_xmpp::commands::Note::info));
                }
            }
            ExtensionEffect::Noop => {}
        }
    }
    if notes.is_empty() {
        notes.push(waddle_xmpp::commands::Note::warn(
            "Extension action completed without a visible result".to_string(),
        ));
    }

    waddle_xmpp::commands::CommandResult::Completed {
        form: result_form,
        notes,
    }
}

const MAX_EXTENSION_PUBSUB_ITEMS: u32 = 500;

async fn publish_extension_pubsub(
    storage: &dyn PubSubStorage,
    owner: &jid::BareJid,
    publish: PubSubPublish,
) -> Result<String, waddle_xmpp::XmppError> {
    ensure_extension_pubsub_node(storage, owner, publish.node.as_str()).await?;
    let item = PubSubItem::new(
        publish.item_id.map(|item_id| item_id.into_string()),
        Some(publish.payload.to_minidom()),
    );
    let result = storage
        .publish_item(owner, publish.node.as_str(), &item, Some(owner), false)
        .await?;
    Ok(result.item_id)
}

async fn ensure_extension_pubsub_node(
    storage: &dyn PubSubStorage,
    owner: &jid::BareJid,
    node: &str,
) -> Result<(), waddle_xmpp::XmppError> {
    let config = extension_pubsub_node_config();
    let (existing, _) = storage.get_or_create_node(owner, node).await?;
    if existing.config != config {
        storage.update_node_config(owner, node, &config).await?;
    }
    storage
        .set_affiliation(owner, node, owner, waddle_xmpp::pubsub::Affiliation::Owner)
        .await?;
    Ok(())
}

fn extension_pubsub_node_config() -> NodeConfig {
    let mut config = NodeConfig::spaces_private();
    config.max_items = MAX_EXTENSION_PUBSUB_ITEMS;
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_extensions::types::{
        DataForm, DataFormType, DisplayText, EnrichmentId, ExtensionCapability, ExtensionEnvelope,
        MessageEnrichment, PayloadNamespace, PluginId, TextBlock, TextStyle, Timestamp, UiBlock,
        UiView, UiViewId,
    };
    use waddle_xmpp::commands::CommandResult;
    use waddle_xmpp::xep::xep0050::NoteType;

    #[tokio::test]
    async fn enrichment_command_result_completes_without_noop_warning() {
        let result =
            extension_command_result(vec![ExtensionEffect::EnrichMessage(envelope())], None, None)
                .await;

        let CommandResult::Completed {
            form: Some(_),
            notes,
        } = result
        else {
            panic!("expected completed command result with visible form");
        };
        assert!(notes.iter().all(|note| note.note_type != NoteType::Warn));
        assert!(notes.iter().any(|note| {
            note.note_type == NoteType::Info && note.text == "AI answer posted to channel."
        }));
    }

    #[tokio::test]
    async fn command_form_result_preserves_session_id() {
        let result = extension_command_result(
            vec![ExtensionEffect::CommandForm(DataForm {
                form_type: DataFormType::Form,
                title: None,
                instructions: Vec::new(),
                fields: Vec::new(),
            })],
            None,
            Some("session-123".to_string()),
        )
        .await;

        let CommandResult::Executing { session_id, .. } = result else {
            panic!("expected executing command result");
        };
        assert_eq!(session_id, "session-123");
    }

    #[tokio::test]
    async fn host_warning_command_result_uses_error_note() {
        let result = extension_command_result(
            vec![ExtensionEffect::HostWarning(
                DisplayText::new("invalid submitted form").expect("warning text"),
            )],
            None,
            Some("session-123".to_string()),
        )
        .await;

        let CommandResult::Completed { notes, .. } = result else {
            panic!("expected completed command result");
        };
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].note_type, NoteType::Error);
        assert_eq!(notes[0].text, "invalid submitted form");
    }

    fn envelope() -> ExtensionEnvelope {
        ExtensionEnvelope::new(vec![MessageEnrichment {
            id: EnrichmentId::new("ai-command-posted").expect("enrichment id"),
            plugin: PluginId::new("ai-chatbot").expect("plugin id"),
            capability: ExtensionCapability::MessageEnrich,
            payload_namespace: PayloadNamespace::framework(),
            created_at: Timestamp::new("2026-05-09T00:00:00Z").expect("timestamp"),
            source: None,
            ui: vec![UiView {
                id: UiViewId::new("ai-command-posted").expect("view id"),
                title: Some(DisplayText::new("AI answer posted").expect("title")),
                blocks: vec![UiBlock::Text(TextBlock {
                    text: DisplayText::new("AI answer posted to channel.").expect("body"),
                    style: TextStyle::Body,
                })],
            }],
            payloads: vec![],
            launches: vec![],
        }])
    }
}
