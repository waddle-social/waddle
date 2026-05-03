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
use bindings::waddle::extension::host_tools;
use bindings::waddle::extension::types;
use core::fmt::Write as _;
use sha2::{Digest, Sha256};

struct DecisionPolls;

bindings::export!(DecisionPolls with_types_in bindings);

const PLUGIN_ID: &str = "decision-polls";
const PLUGIN_NAME: &str = "Decision Polls";
const PLUGIN_NS: &str = "urn:waddle:decision-polls:1";
const FRAMEWORK_NS: &str = "urn:waddle:extension:1";
const EXTENSION_ITEM_ROOT: &str = "extension-item";
const INVOKE_NODE: &str = "urn:waddle:extension:1:invoke";
const COMMAND_NODE: &str = "urn:waddle:extension:1:decision-polls";
const VERSION: &str = "0.1.0";

impl exports::waddle::extension::lifecycle::Guest for DecisionPolls {
    fn init(_config: String) -> Result<types::ExtensionManifest, String> {
        Ok(manifest())
    }
}

impl exports::waddle::extension::framework::Guest for DecisionPolls {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::MessageHook(_) => vec![],
            types::ExtensionEvent::Command(command) => handle_command(command)?,
            types::ExtensionEvent::Launch(launch) => handle_vote(launch),
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
            payload_rule(types::PayloadSurface::MessageEnrichment, "poll"),
            payload_rule(types::PayloadSurface::MessageEnrichment, "vote-ack"),
            payload_rule(types::PayloadSurface::LaunchPayload, "vote-request"),
            payload_rule(types::PayloadSurface::PubsubItem, "poll"),
            payload_rule(types::PayloadSurface::PubsubItem, "results"),
            payload_rule(types::PayloadSurface::PubsubItem, "vote"),
        ],
        capabilities: vec![
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::Commands,
            types::ExtensionCapability::Launch,
            types::ExtensionCapability::PubsubPublish,
            types::ExtensionCapability::HostMessageSend,
            types::ExtensionCapability::UiDeclarative,
        ],
        commands: vec![
            command_descriptor(
                COMMAND_NODE,
                "Create Decision Poll",
                types::CommandScope::Channel,
            ),
            command_descriptor(
                INVOKE_NODE,
                "Run Decision Poll action",
                types::CommandScope::Channel,
            ),
        ],
        routes: vec![types::ExtensionRouteDescriptor {
            plugin: plugin_id(),
            id: types::RouteId {
                value: "polls".to_string(),
            },
            label: display("Polls"),
            scope: types::ExtensionRouteScope::Channel,
            surface: types::ExtensionRouteSurface::ListView,
            state_node: polls_node_template(),
            payload_namespace: payload_namespace(),
        }],
        pubsub_nodes: vec![
            polls_node_template(),
            results_node_template(),
            votes_node_template(),
        ],
        artifact: None,
    }
}

fn handle_command(
    command: types::CommandInvocation,
) -> Result<Vec<types::ExtensionEffect>, types::ExtensionError> {
    if command.command_node.value != COMMAND_NODE {
        return Ok(vec![]);
    }
    if matches!(command.action, Some(types::CommandAction::Cancel)) {
        return Ok(vec![]);
    }
    if !matches!(
        command.action,
        Some(types::CommandAction::Complete) | Some(types::CommandAction::Next)
    ) || field_value(&command.fields, "question").is_none()
    {
        return Ok(vec![
            types::ExtensionEffect::CommandForm(create_poll_form()),
        ]);
    }

    let Some(room) = command.room.clone() else {
        return Ok(vec![types::ExtensionEffect::HostWarning(display(
            "Decision polls require an active channel.",
        ))]);
    };
    let question = required_field(&command.fields, "question")?;
    let options = poll_options(&command.fields)?;
    let duration = duration_seconds(
        &field_value(&command.fields, "duration").unwrap_or_else(|| "1h".to_string()),
    )?;
    let poll_id = command
        .session_id
        .as_ref()
        .map(|id| id.value.clone())
        .unwrap_or_else(|| "poll".to_string());
    let closes_at = closes_at(duration);
    let poll = Poll {
        poll_id,
        question,
        options,
        closes_at,
        room,
        waddle_id: command.waddle_id,
    };

    send_poll_message(&poll)?;
    Ok(vec![types::ExtensionEffect::PublishPubsub(
        types::PubsubPublish {
            node: polls_node(&poll.room),
            item_id: Some(types::PubsubItemId {
                value: poll.poll_id.clone(),
            }),
            payload: poll_extension_item(&poll),
        },
    )])
}

fn handle_vote(launch: types::LaunchInvocation) -> Vec<types::ExtensionEffect> {
    let Some(room) = launch.context.room.clone() else {
        return vec![types::ExtensionEffect::HostWarning(display(
            "Poll votes require a channel context.",
        ))];
    };
    let poll_id = field_value(&launch.fields, "payload#vote-request#poll-id")
        .unwrap_or_else(|| "poll".to_string());
    let option_id = field_value(&launch.fields, "payload#vote-request#option-id")
        .unwrap_or_else(|| launch.launch_id.value.clone());
    let voter = stable_id(bare_jid_value(&launch.requester.value));
    vec![
        types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
            node: votes_node(&room, &poll_id),
            item_id: Some(types::PubsubItemId { value: voter }),
            payload: vote_extension_item(&poll_id, &option_id),
        }),
        types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
            node: results_node(&room),
            item_id: Some(types::PubsubItemId {
                value: poll_id.clone(),
            }),
            payload: results_extension_item(&poll_id, &option_id),
        }),
    ]
}

struct Poll {
    poll_id: String,
    question: String,
    options: Vec<PollOption>,
    closes_at: String,
    room: types::RoomJid,
    waddle_id: types::WaddleId,
}

struct PollOption {
    id: String,
    label: String,
}

fn create_poll_form() -> types::DataForm {
    types::DataForm {
        form_type: types::DataFormType::Form,
        title: Some(display("Create Poll")),
        instructions: vec![display(
            "Enter a question, one option per line, and a time limit.",
        )],
        fields: vec![
            types::DataFormField {
                name: action_id("question"),
                field_type: types::FormFieldType::TextSingle,
                label: Some(display("Question")),
                required: true,
                values: vec![],
                options: vec![],
            },
            types::DataFormField {
                name: action_id("options"),
                field_type: types::FormFieldType::TextMulti,
                label: Some(display("Options")),
                required: true,
                values: vec![],
                options: vec![],
            },
            types::DataFormField {
                name: action_id("duration"),
                field_type: types::FormFieldType::ListSingle,
                label: Some(display("Duration")),
                required: true,
                values: vec![form_value("1h")],
                options: vec![
                    form_option("15 minutes", "15m"),
                    form_option("1 hour", "1h"),
                    form_option("1 day", "1d"),
                    form_option("1 week", "1w"),
                ],
            },
        ],
    }
}

fn send_poll_message(poll: &Poll) -> Result<(), types::ExtensionError> {
    host_tools::send_message(&types::SendMessageRequest {
        target: types::MessageTarget::Muc(poll.room.clone()),
        body: display(&format!("Poll: {}", poll.question)),
        thread_id: None,
        reply_to: None,
        extensions: Some(types::ExtensionEnvelope {
            version: 1,
            enrichments: vec![poll_enrichment(poll)],
        }),
    })
    .map(|_| ())
    .map_err(extension_error_from_host_tool)
}

fn poll_enrichment(poll: &Poll) -> types::MessageEnrichment {
    types::MessageEnrichment {
        id: types::EnrichmentId {
            value: format!("poll-{}", poll.poll_id),
        },
        plugin: plugin_id(),
        capability: types::ExtensionCapability::MessageEnrich,
        payload_namespace: payload_namespace(),
        created_at: timestamp(),
        source: None,
        ui: vec![poll_view(poll)],
        payloads: vec![poll_payload(poll)],
        launches: vote_launches(poll),
    }
}

fn poll_view(poll: &Poll) -> types::UiView {
    let mut blocks = vec![types::UiBlock::Text(types::TextBlock {
        text: display(&poll.question),
        style: types::TextStyle::Body,
    })];
    for option in &poll.options {
        blocks.push(types::UiBlock::Action(types::ActionBlock {
            launch_id: types::LaunchId {
                value: format!("vote-{}", option.id),
            },
            label: display(&option.label),
        }));
    }
    types::UiView {
        id: types::UiViewId {
            value: format!("poll-{}", poll.poll_id),
        },
        title: Some(display(PLUGIN_NAME)),
        blocks,
    }
}

fn vote_launches(poll: &Poll) -> Vec<types::LaunchDescriptor> {
    poll.options
        .iter()
        .map(|option| types::LaunchDescriptor {
            id: types::LaunchId {
                value: format!("vote-{}", option.id),
            },
            plugin: plugin_id(),
            action: types::ActionId {
                value: "vote".to_string(),
            },
            command_node: types::CommandNode {
                value: INVOKE_NODE.to_string(),
            },
            label: display(&option.label),
            context: types::LaunchContext {
                waddle_id: poll.waddle_id.clone(),
                room: Some(poll.room.clone()),
                source_stanza_id: None,
            },
            payloads: vec![payload(
                "vote-request",
                vec![
                    ("poll-id", poll.poll_id.clone()),
                    ("option-id", option.id.clone()),
                    ("closes-at", poll.closes_at.clone()),
                ],
                &option.label,
            )],
            fallback: None,
            expires_at: Some(types::Timestamp {
                value: poll.closes_at.clone(),
            }),
        })
        .collect()
}

fn poll_payload(poll: &Poll) -> types::ExtensionPayload {
    let mut attrs = vec![
        ("poll-id", poll.poll_id.clone()),
        ("mode", "single".to_string()),
        ("closes-at", poll.closes_at.clone()),
        ("option-count", poll.options.len().to_string()),
    ];
    for (index, option) in poll.options.iter().enumerate() {
        attrs.push((option_key(index, "id"), option.id.clone()));
        attrs.push((option_key(index, "label"), option.label.clone()));
    }
    payload("poll", attrs, &poll.question)
}

fn poll_extension_item(poll: &Poll) -> types::ExtensionPayload {
    let mut item = ExtensionItem::new();
    item.with_title(&poll.question);
    item.with_subtitle(&format!("Closes at {}", poll.closes_at));
    for option in &poll.options {
        item.with_option(&option.id, &option.label);
    }
    item.with_action(&format!("vote-{}", poll.poll_id), "Vote");
    item.into_payload()
}

fn vote_extension_item(poll_id: &str, option_id: &str) -> types::ExtensionPayload {
    let mut item = ExtensionItem::new();
    item.with_title("Vote recorded");
    item.with_field("poll-id", "Poll", poll_id);
    item.with_field("option-id", "Choice", option_id);
    item.into_payload()
}

fn results_extension_item(poll_id: &str, latest_option_id: &str) -> types::ExtensionPayload {
    let mut item = ExtensionItem::new();
    item.with_title(&format!("Results for {poll_id}"));
    item.with_subtitle(&format!("Latest vote: {latest_option_id}"));
    item.with_field("poll-id", "Poll", poll_id);
    item.with_field("latest-option-id", "Latest choice", latest_option_id);
    item.into_payload()
}

fn option_key(index: usize, field: &str) -> &'static str {
    match (index, field) {
        (0, "id") => "option-0-id",
        (0, "label") => "option-0-label",
        (1, "id") => "option-1-id",
        (1, "label") => "option-1-label",
        (2, "id") => "option-2-id",
        (2, "label") => "option-2-label",
        (3, "id") => "option-3-id",
        (3, "label") => "option-3-label",
        (4, "id") => "option-4-id",
        (4, "label") => "option-4-label",
        _ => "option-extra",
    }
}

fn poll_options(
    fields: &[types::FormFieldValue],
) -> Result<Vec<PollOption>, types::ExtensionError> {
    let raw_options = field_values(fields, "options");
    if raw_options.is_empty() {
        return Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "missing required field options",
        ));
    }
    let options = raw_options
        .iter()
        .flat_map(|value| value.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(5)
        .enumerate()
        .map(|(index, label)| PollOption {
            id: format!("option-{}", index + 1),
            label: label.to_string(),
        })
        .collect::<Vec<_>>();
    if options.len() < 2 {
        return Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "polls require at least two options",
        ));
    }
    Ok(options)
}

fn duration_seconds(value: &str) -> Result<i64, types::ExtensionError> {
    match value {
        "15m" => Ok(15 * 60),
        "1h" => Ok(60 * 60),
        "1d" => Ok(24 * 60 * 60),
        "1w" => Ok(7 * 24 * 60 * 60),
        _ => Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "unsupported poll duration",
        )),
    }
}

fn closes_at(duration_seconds: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(duration_seconds)).to_rfc3339()
}

fn required_field(
    fields: &[types::FormFieldValue],
    name: &str,
) -> Result<String, types::ExtensionError> {
    field_value(fields, name)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            extension_error(
                types::ExtensionErrorCode::InvalidRequest,
                &format!("missing required field {name}"),
            )
        })
}

fn field_value(fields: &[types::FormFieldValue], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|field| field.name.value == name)
        .and_then(|field| field.values.first())
        .map(|value| value.value.clone())
}

fn field_values(fields: &[types::FormFieldValue], name: &str) -> Vec<String> {
    fields
        .iter()
        .find(|field| field.name.value == name)
        .map(|field| {
            field
                .values
                .iter()
                .map(|value| value.value.clone())
                .collect()
        })
        .unwrap_or_default()
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

fn payload_rule(surface: types::PayloadSurface, root: &str) -> types::PayloadRule {
    types::PayloadRule {
        surface,
        root: types::PayloadRoot {
            namespace: payload_namespace(),
            local_name: root.to_string(),
        },
    }
}

fn command_descriptor(
    node: &str,
    name: &str,
    scope: types::CommandScope,
) -> types::CommandDescriptor {
    types::CommandDescriptor {
        node: types::CommandNode {
            value: node.to_string(),
        },
        name: display(name),
        scope,
    }
}

fn polls_node_template() -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{{room}}:polls"),
    }
}

fn results_node_template() -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{{room}}:results"),
    }
}

fn votes_node_template() -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{{room}}:votes:{{poll-id}}"),
    }
}

fn polls_node(room: &types::RoomJid) -> types::PubsubNode {
    typed_node(room, "polls")
}

fn results_node(room: &types::RoomJid) -> types::PubsubNode {
    typed_node(room, "results")
}

fn votes_node(room: &types::RoomJid, poll_id: &str) -> types::PubsubNode {
    typed_node(room, &format!("votes:{poll_id}"))
}

fn typed_node(room: &types::RoomJid, suffix: &str) -> types::PubsubNode {
    types::PubsubNode {
        value: format!("{PLUGIN_NS}:channel:{}:{suffix}", room.value),
    }
}

fn form_option(label: &str, value: &str) -> types::FormFieldOption {
    types::FormFieldOption {
        label: Some(display(label)),
        value: form_value(value),
    }
}

fn form_value(value: &str) -> types::DataFormValue {
    types::DataFormValue {
        value: value.to_string(),
    }
}

fn action_id(value: &str) -> types::UiActionId {
    types::UiActionId {
        value: value.to_string(),
    }
}

fn stable_id(value: &str) -> String {
    hashed_id("voter", value)
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

fn bare_jid_value(value: &str) -> &str {
    value.split_once('/').map_or(value, |(bare, _)| bare)
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

fn extension_error(code: types::ExtensionErrorCode, message: &str) -> types::ExtensionError {
    types::ExtensionError {
        code,
        message: display(message),
    }
}

fn extension_error_from_host_tool(error: types::HostToolError) -> types::ExtensionError {
    let code = match error.code {
        types::HostToolErrorCode::Denied => types::ExtensionErrorCode::Denied,
        types::HostToolErrorCode::InvalidRequest => types::ExtensionErrorCode::InvalidRequest,
        types::HostToolErrorCode::NotFound => types::ExtensionErrorCode::InvalidRequest,
        types::HostToolErrorCode::Unsupported => types::ExtensionErrorCode::UnsupportedEvent,
        types::HostToolErrorCode::TemporaryFailure => types::ExtensionErrorCode::TemporaryFailure,
    };
    types::ExtensionError {
        code,
        message: error.message,
    }
}

/// Builder for the generic `<extension-item xmlns="urn:waddle:extension:1">`
/// envelope that every Waddle extension publishes for its PubSub state items.
struct ExtensionItem {
    children: Vec<types::XmlToken>,
}

impl ExtensionItem {
    fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    fn with_title(&mut self, value: &str) -> &mut Self {
        self.push_text_element("title", value);
        self
    }

    fn with_subtitle(&mut self, value: &str) -> &mut Self {
        self.push_text_element("subtitle", value);
        self
    }

    fn with_field(&mut self, name: &str, label: &str, value: &str) -> &mut Self {
        self.push_text_element_with_attrs(
            "field",
            vec![("name", name.to_string()), ("label", label.to_string())],
            value,
        );
        self
    }

    fn with_option(&mut self, id: &str, label: &str) -> &mut Self {
        self.push_empty_element(
            "option",
            vec![("id", id.to_string()), ("label", label.to_string())],
        );
        self
    }

    fn with_action(&mut self, launch_id_value: &str, label: &str) -> &mut Self {
        self.push_empty_element(
            "action",
            vec![
                ("launch-id", launch_id_value.to_string()),
                ("label", label.to_string()),
            ],
        );
        self
    }

    fn into_payload(self) -> types::ExtensionPayload {
        let namespace = framework_namespace();
        let mut tokens = Vec::with_capacity(self.children.len() + 2);
        tokens.push(types::XmlToken::StartElement(types::XmlElement {
            namespace: namespace.clone(),
            local_name: EXTENSION_ITEM_ROOT.to_string(),
            attributes: Vec::new(),
        }));
        tokens.extend(self.children);
        tokens.push(types::XmlToken::EndElement);
        types::ExtensionPayload {
            namespace: namespace.clone(),
            root: types::PayloadRoot {
                namespace,
                local_name: EXTENSION_ITEM_ROOT.to_string(),
            },
            tokens,
        }
    }

    fn push_text_element(&mut self, local_name: &str, text: &str) {
        self.push_text_element_with_attrs(local_name, Vec::new(), text);
    }

    fn push_text_element_with_attrs(
        &mut self,
        local_name: &str,
        attrs: Vec<(&str, String)>,
        text: &str,
    ) {
        self.children
            .push(types::XmlToken::StartElement(framework_xml_element(
                local_name, attrs,
            )));
        self.children.push(types::XmlToken::Text(text.to_string()));
        self.children.push(types::XmlToken::EndElement);
    }

    fn push_empty_element(&mut self, local_name: &str, attrs: Vec<(&str, String)>) {
        self.children
            .push(types::XmlToken::StartElement(framework_xml_element(
                local_name, attrs,
            )));
        self.children.push(types::XmlToken::EndElement);
    }
}

fn framework_xml_element(local_name: &str, attrs: Vec<(&str, String)>) -> types::XmlElement {
    types::XmlElement {
        namespace: framework_namespace(),
        local_name: local_name.to_string(),
        attributes: attrs
            .into_iter()
            .map(|(name, value)| types::XmlAttribute {
                namespace: None,
                local_name: name.to_string(),
                value,
            })
            .collect(),
    }
}

fn framework_namespace() -> types::PayloadNamespace {
    types::PayloadNamespace {
        value: FRAMEWORK_NS.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_decision_poll_commands_as_channel_scoped() {
        let manifest = manifest();
        assert_eq!(manifest.commands.len(), 2);
        for command in &manifest.commands {
            assert!(
                matches!(command.scope, types::CommandScope::Channel),
                "command {} should require an active channel context",
                command.node.value,
            );
        }
    }

    #[test]
    fn poll_options_accept_multiple_xep0004_values() {
        let options = poll_options(&[form_field("options", &["Ship it", "Revise it", "Block it"])])
            .expect("valid options");

        assert_eq!(
            option_labels(&options),
            vec!["Ship it", "Revise it", "Block it"]
        );
    }

    #[test]
    fn poll_options_accept_newline_delimited_value() {
        let options = poll_options(&[form_field("options", &["Ship it\nRevise it\n\nBlock it"])])
            .expect("valid options");

        assert_eq!(
            option_labels(&options),
            vec!["Ship it", "Revise it", "Block it"]
        );
    }

    fn form_field(name: &str, values: &[&str]) -> types::FormFieldValue {
        types::FormFieldValue {
            name: types::UiActionId {
                value: name.to_string(),
            },
            values: values
                .iter()
                .map(|value| types::DataFormValue {
                    value: value.to_string(),
                })
                .collect(),
        }
    }

    fn option_labels(options: &[PollOption]) -> Vec<&str> {
        options.iter().map(|option| option.label.as_str()).collect()
    }
}
