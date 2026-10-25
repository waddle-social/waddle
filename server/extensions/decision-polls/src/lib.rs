mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "waddle-extension",
        with: {
            "wasi:logging/logging@0.1.0-draft": generate,
            "wasi:clocks/monotonic-clock@0.2.0": generate,
            "wasi:io/poll@0.2.0": generate,
            "wasi:random/random@0.2.0": generate,
        },
    });
}

use bindings::exports;
use bindings::waddle::extension::types;

mod command;
mod helpers;
mod manifest;
mod payloads;
mod poll;

use command::{handle_command, handle_vote};
use helpers::{
    action_id, bare_jid_value, closes_at, command_descriptor, display, duration_seconds,
    extension_error_from_host_tool, field_value, form_option, form_value, option_key, payload,
    payload_namespace, payload_rule, plugin_id, poll_options, polls_node, polls_node_template,
    required_field, results_node, results_node_template, stable_id, timestamp, votes_node,
    votes_node_template,
};
use manifest::manifest;
use payloads::{poll_extension_item, results_extension_item, vote_extension_item};
use poll::{create_poll_form, send_poll_message, Poll, PollOption};

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
        _delivery: Option<&bindings::waddle::extension::delivery::DeliveryKey>,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::RoomMessageObserve(_) => vec![],
            types::ExtensionEvent::MessageHook(_) => vec![],
            types::ExtensionEvent::Command(command) => handle_command(command)?,
            types::ExtensionEvent::Launch(launch) => handle_vote(launch),
            types::ExtensionEvent::ProviderWebhook(_) => vec![],
        };
        Ok(types::ExtensionResponse {
            effects,
            usage: None,
        })
    }
}

#[cfg(test)]
mod tests;
