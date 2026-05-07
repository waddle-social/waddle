use super::*;

use crate::bindings::waddle::extension::host_tools;

pub(super) struct Poll {
    pub(super) poll_id: String,
    pub(super) question: String,
    pub(super) options: Vec<PollOption>,
    pub(super) closes_at: String,
    pub(super) room: types::RoomJid,
    pub(super) waddle_id: types::WaddleId,
}

pub(super) struct PollOption {
    pub(super) id: String,
    pub(super) label: String,
}

pub(super) fn create_poll_form() -> types::DataForm {
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

pub(super) fn send_poll_message(poll: &Poll) -> Result<(), types::ExtensionError> {
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

pub(super) fn poll_enrichment(poll: &Poll) -> types::MessageEnrichment {
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

pub(super) fn poll_view(poll: &Poll) -> types::UiView {
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

pub(super) fn vote_launches(poll: &Poll) -> Vec<types::LaunchDescriptor> {
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

pub(super) fn poll_payload(poll: &Poll) -> types::ExtensionPayload {
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
