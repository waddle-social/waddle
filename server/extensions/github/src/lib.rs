mod bindings;

use bindings::exports;
#[cfg(not(test))]
use bindings::waddle::extension::host_tools;
#[cfg(not(test))]
use bindings::waddle::extension::runtime;
use bindings::waddle::extension::types;
use serde::Deserialize;

bindings::export!(GitHub with_types_in bindings);

#[cfg(test)]
fn load_routes() -> Result<Vec<Route>, types::ExtensionError> {
    Ok(test_state::route_fixtures())
}

#[cfg(test)]
fn current_config() -> Result<GitHubConfig, types::ExtensionError> {
    Ok(test_state::config_fixture())
}

#[cfg(test)]
fn send_message_request(request: &types::SendMessageRequest) -> Result<(), types::ExtensionError> {
    test_state::sent_room_messages()
        .lock()
        .expect("sent room messages lock")
        .push(request.clone());
    Ok(())
}

#[cfg(test)]
fn current_timestamp_value() -> String {
    "2026-05-12T00:00:00Z".to_string()
}

#[cfg(test)]
mod test_state;

#[cfg(test)]
mod tests;

mod extension;
use extension::alert_for_payload;
use extension::bare_jid_value;
use extension::build_route_from_fields;
use extension::command_descriptor;
use extension::configure_route_form;
#[cfg(not(test))]
use extension::current_config;
#[cfg(not(test))]
use extension::current_timestamp_value;
use extension::display;
use extension::extension_error;
#[cfg(not(test))]
use extension::extension_error_from_host_tool;
use extension::field_value;
use extension::field_values;
use extension::find_attr;
use extension::form_option;
use extension::form_value;
use extension::github_envelope;
use extension::handle_command;
use extension::handle_provider_webhook;
use extension::is_deployment_alert_state;
use extension::is_workflow_check_alert_conclusion;
#[cfg(not(test))]
use extension::load_routes;
use extension::manifest;
use extension::migrate_route;
use extension::parse_config;
use extension::payload_namespace;
use extension::payload_rule;
use extension::plugin_id;
use extension::require_field;
use extension::require_form_type;
use extension::require_numeric_field;
#[cfg(not(test))]
use extension::send_message_request;
use extension::send_room_message;
use extension::should_alert_for_payload;
use extension::status_branch;
use extension::status_conclusion;
use extension::status_name;
use extension::status_payload_prefix;
use extension::status_revision;
use extension::status_url;
use extension::timestamp;
use extension::xml_attr;
use extension::GitHub;
use extension::GitHubAlert;
use extension::GitHubConfig;
use extension::GitHubPayload;
use extension::MigratedRoute;
use extension::ProviderFields;
use extension::RawConfig;
use extension::Route;
use extension::ATTR_CHANNEL;
use extension::ATTR_EVENTS;
use extension::ATTR_INSTALLATION_ID;
use extension::ATTR_REPOSITORY_ID;
use extension::COMMAND_NODE;
use extension::CONFIGURE_ROUTE_FORM_TYPE;
use extension::FIELD_CHANNEL;
use extension::FIELD_EVENTS;
use extension::FIELD_FORM_TYPE;
use extension::FIELD_INSTALLATION_ID;
use extension::FIELD_REPOSITORY_ID;
use extension::GITHUB_ACTIONS_ALERT_ROOM;
use extension::LEGACY_ALERT_ROOM;
use extension::PLUGIN_ID;
use extension::PLUGIN_NAME;
use extension::PLUGIN_NS;
use extension::ROUTES_NODE;
use extension::ROUTE_ELEMENT;
use extension::VERSION;
