mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "waddle-extension",
        with: {
            "wasi:logging/logging@0.1.0-draft": generate,
            "wasi:clocks/monotonic-clock@0.2.0": generate,
            "wasi:io/poll@0.2.0": generate,
        },
    });
}

use bindings::exports;
use bindings::waddle::extension::types;
use core::fmt::Write as _;
use sha2::{Digest, Sha256};

struct LinkBoard;

bindings::export!(LinkBoard with_types_in bindings);

const PLUGIN_ID: &str = "link-board";
const PLUGIN_NAME: &str = "Link Board";
const PLUGIN_NS: &str = "urn:waddle:link-board:1";
const INVOKE_NODE: &str = "urn:waddle:extension:1:invoke";
const VERSION: &str = "0.1.0";

impl exports::waddle::extension::lifecycle::Guest for LinkBoard {
    fn init(_config: String) -> Result<types::ExtensionManifest, String> {
        Ok(manifest())
    }
}

impl exports::waddle::extension::framework::Guest for LinkBoard {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::MessageHook(hook) => link_enrichments(&hook),
            types::ExtensionEvent::Launch(launch) => save_link(launch),
            types::ExtensionEvent::Command(_) => vec![],
        };
        Ok(types::ExtensionResponse { effects })
    }
}

fn manifest() -> types::ExtensionManifest {
    types::ExtensionManifest {
        id: plugin_id(),
        name: display(PLUGIN_NAME),
        version: types::PluginVersion {
            value: VERSION.to_string(),
        },
        payloads: vec![
            payload_rule(types::PayloadSurface::MessageEnrichment, "link"),
            payload_rule(types::PayloadSurface::LaunchPayload, "link"),
            payload_rule(types::PayloadSurface::PubsubItem, "link"),
        ],
        capabilities: vec![
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::Launch,
            types::ExtensionCapability::PubsubPublish,
            types::ExtensionCapability::UiDeclarative,
        ],
        commands: vec![command_descriptor(INVOKE_NODE, "Run Link Board action")],
        routes: vec![types::ExtensionRouteDescriptor {
            plugin: plugin_id(),
            id: types::RouteId {
                value: "saved-links".to_string(),
            },
            label: display("Saved Links"),
            scope: types::ExtensionRouteScope::Channel,
            surface: types::ExtensionRouteSurface::Gallery,
            state_node: links_node_template(),
            payload_namespace: payload_namespace(),
        }],
        pubsub_nodes: vec![links_node_template()],
        artifact: None,
    }
}

fn link_enrichments(hook: &types::MessageHook) -> Vec<types::ExtensionEffect> {
    let Some(room) = hook.context.room.clone() else {
        return vec![];
    };
    if hook.links.is_empty() {
        return vec![];
    }
    vec![types::ExtensionEffect::EnrichMessage(
        types::ExtensionEnvelope {
            version: 1,
            enrichments: hook
                .links
                .iter()
                .enumerate()
                .map(|(index, link)| link_enrichment(hook, &room, index, link))
                .collect(),
        },
    )]
}

fn link_enrichment(
    hook: &types::MessageHook,
    room: &types::RoomJid,
    index: usize,
    link: &types::LinkTarget,
) -> types::MessageEnrichment {
    let url = link.url.value.clone();
    let launch_id_value = format!("save-link-{index}");
    types::MessageEnrichment {
        id: enrichment_id(&format!("link-{index}")),
        plugin: plugin_id(),
        capability: types::ExtensionCapability::MessageEnrich,
        payload_namespace: payload_namespace(),
        created_at: timestamp(),
        source: hook
            .context
            .stanza_id
            .clone()
            .map(|stanza_id| types::MessageSource {
                stanza_id,
                body_range: Some(link.range),
            }),
        ui: vec![view(
            &format!("link-card-{index}"),
            PLUGIN_NAME,
            &format!("Save {url}"),
            &launch_id_value,
        )],
        payloads: vec![link_payload(
            &url,
            &normalized_url(&url),
            hook.context.stanza_id.as_ref(),
            Some(&link.range),
        )],
        launches: vec![launch(
            &launch_id_value,
            "save-link",
            "Save",
            &hook.context,
            room,
            &url,
            link.range,
        )],
    }
}

fn save_link(launch: types::LaunchInvocation) -> Vec<types::ExtensionEffect> {
    let Some(room) = launch.context.room.clone() else {
        return vec![types::ExtensionEffect::HostWarning(display(
            "Link Board saves require a channel context.",
        ))];
    };
    let Some(url) = field_value(&launch.fields, "payload#link#url") else {
        return vec![types::ExtensionEffect::HostWarning(display(
            "Link Board save action is missing a URL.",
        ))];
    };
    let normalized = field_value(&launch.fields, "payload#link#normalized-url")
        .unwrap_or_else(|| normalized_url(&url));
    let source = launch.context.source_stanza_id.as_ref();
    vec![types::ExtensionEffect::PublishPubsub(
        types::PubsubPublish {
            node: links_node(&room),
            item_id: Some(types::PubsubItemId {
                value: link_item_id(&normalized),
            }),
            payload: link_payload(&url, &normalized, source, None),
        },
    )]
}

fn launch(
    id_value: &str,
    action: &str,
    label: &str,
    context: &types::MessageContext,
    room: &types::RoomJid,
    url: &str,
    range: types::BodyRange,
) -> types::LaunchDescriptor {
    types::LaunchDescriptor {
        id: launch_id(id_value),
        plugin: plugin_id(),
        action: types::ActionId {
            value: action.to_string(),
        },
        command_node: types::CommandNode {
            value: INVOKE_NODE.to_string(),
        },
        label: display(label),
        context: types::LaunchContext {
            waddle_id: context.waddle_id.clone(),
            room: Some(room.clone()),
            source_stanza_id: context.stanza_id.clone(),
        },
        payloads: vec![link_payload(
            url,
            &normalized_url(url),
            context.stanza_id.as_ref(),
            Some(&range),
        )],
        fallback: None,
        expires_at: None,
    }
}

fn view(id_value: &str, title: &str, text: &str, launch_id_value: &str) -> types::UiView {
    types::UiView {
        id: types::UiViewId {
            value: id_value.to_string(),
        },
        title: Some(display(title)),
        blocks: vec![
            types::UiBlock::Text(types::TextBlock {
                text: display(text),
                style: types::TextStyle::Body,
            }),
            types::UiBlock::Action(types::ActionBlock {
                launch_id: launch_id(launch_id_value),
                label: display("Save"),
            }),
        ],
    }
}

fn link_payload(
    url: &str,
    normalized: &str,
    source_stanza_id: Option<&types::StanzaId>,
    range: Option<&types::BodyRange>,
) -> types::ExtensionPayload {
    let mut attrs = vec![
        ("url", url.to_string()),
        ("normalized-url", normalized.to_string()),
    ];
    if let Some(source_stanza_id) = source_stanza_id {
        attrs.push(("source-stanza-id", source_stanza_id.value.clone()));
    }
    if let Some(range) = range {
        attrs.push(("body-start", range.start.to_string()));
        attrs.push(("body-end", range.end.to_string()));
    }
    payload("link", attrs, url)
}

fn payload(root: &str, attrs: Vec<(&str, String)>, text: &str) -> types::ExtensionPayload {
    let namespace = payload_namespace();
    types::ExtensionPayload {
        namespace: namespace.clone(),
        root: types::PayloadRoot {
            namespace: namespace.clone(),
            local_name: root.to_string(),
        },
        tokens: vec![
            types::XmlToken::StartElement(types::XmlElement {
                namespace,
                local_name: root.to_string(),
                attributes: attrs
                    .into_iter()
                    .map(|(name, value)| types::XmlAttribute {
                        namespace: None,
                        local_name: name.to_string(),
                        value,
                    })
                    .collect(),
            }),
            types::XmlToken::Text(text.to_string()),
            types::XmlToken::EndElement,
        ],
    }
}

fn field_value(fields: &[types::FormFieldValue], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|field| field.name.value == name)
        .and_then(|field| field.values.first())
        .map(|value| value.value.clone())
}

fn payload_rule(surface: types::PayloadSurface, root: &str) -> types::PayloadRule {
    types::PayloadRule {
        surface,
        root: types::PayloadRoot {
            namespace: payload_namespace(),
            local_name: root.to_string(),
        },
    }
}

fn command_descriptor(node: &str, name: &str) -> types::CommandDescriptor {
    types::CommandDescriptor {
        node: types::CommandNode {
            value: node.to_string(),
        },
        name: display(name),
    }
}

fn links_node_template() -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{{room}}:links"),
    }
}

fn links_node(room: &types::RoomJid) -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{}:links", room.value),
    }
}

fn normalized_url(url: &str) -> String {
    let trimmed = url.trim();
    if let Some(without_hash) = trimmed.split('#').next() {
        without_hash.to_ascii_lowercase()
    } else {
        trimmed.to_ascii_lowercase()
    }
}

fn link_item_id(normalized: &str) -> String {
    hashed_id("link", normalized)
}

fn hashed_id(prefix: &str, value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut id = String::with_capacity(prefix.len() + 65);
    id.push_str(prefix);
    id.push('-');
    for byte in digest {
        let _ = write!(&mut id, "{byte:02x}");
    }
    id
}

fn enrichment_id(value: &str) -> types::EnrichmentId {
    types::EnrichmentId {
        value: value.to_string(),
    }
}

fn launch_id(value: &str) -> types::LaunchId {
    types::LaunchId {
        value: value.to_string(),
    }
}

fn plugin_id() -> types::PluginId {
    types::PluginId {
        value: PLUGIN_ID.to_string(),
    }
}

fn payload_namespace() -> types::PayloadNamespace {
    types::PayloadNamespace {
        value: PLUGIN_NS.to_string(),
    }
}

fn display(value: &str) -> types::DisplayText {
    types::DisplayText {
        value: value.to_string(),
    }
}

fn timestamp() -> types::Timestamp {
    types::Timestamp {
        value: "2026-04-27T00:00:00Z".to_string(),
    }
}
