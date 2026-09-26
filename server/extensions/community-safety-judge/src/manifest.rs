use crate::bindings::waddle::extension::types;
use crate::constants::{JOB_KIND_MESSAGE_JUDGE, PLUGIN_NAME, VERSION};
use crate::ui::{display, plugin_id};

pub(crate) fn manifest() -> types::ExtensionManifest {
    types::ExtensionManifest {
        id: plugin_id(),
        name: display(PLUGIN_NAME),
        version: types::PluginVersion {
            value: VERSION.to_string(),
        },
        payloads: vec![],
        capabilities: vec![
            types::ExtensionCapability::DurableJob,
            types::ExtensionCapability::OutboundHttpRequest,
        ],
        commands: vec![],
        routes: vec![],
        pubsub_nodes: vec![],
        profile: Some(types::ExtensionProfile {
            display_name: display(PLUGIN_NAME),
            description: Some(display(
                "Community safety and enrichment judgments (is_question, hate speech, \
                 explicit content, harassment, violence, self-harm), scored per message via \
                 TypeSafe AI's Jev decision model through OpenRouter.",
            )),
            accent: Some("slate".to_string()),
            avatar: None,
            bot_hat_label: None,
        }),
        artifact: None,
        durable_job_kinds: vec![types::JobKind {
            value: JOB_KIND_MESSAGE_JUDGE.to_string(),
        }],
    }
}
