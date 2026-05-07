use super::*;

pub(super) const EXTENSION_ROUTE_FORM_TYPE: &str = "urn:waddle:extension:1:routes";
pub(super) const EXTENSION_COMMAND_FORM_TYPE: &str = "urn:waddle:extension:1:command";

pub(super) fn is_extension_command_node(node: &str) -> bool {
    node == waddle_extensions::INVOKE_COMMAND_NODE || node.starts_with("urn:waddle:extension:1:")
}

pub(super) fn command_refs_by_boundary(
    commands: &[(String, String)],
    extension_boundary: bool,
) -> Vec<(&str, &str)> {
    commands
        .iter()
        .filter(|(node, _)| is_extension_command_node(node) == extension_boundary)
        .map(|(node, name)| (node.as_str(), name.as_str()))
        .collect()
}

pub(super) fn command_name_by_boundary<'a>(
    commands: &'a [(String, String)],
    node: &str,
    extension_boundary: bool,
) -> Option<&'a str> {
    commands
        .iter()
        .find(|(command_node, _)| {
            command_node == node && is_extension_command_node(command_node) == extension_boundary
        })
        .map(|(_, name)| name.as_str())
}

pub(super) fn extension_route_disco_node(
    route: &waddle_extensions::ExtensionRouteDescriptor,
) -> String {
    format!(
        "urn:waddle:extension:1:route:{}:{}",
        route.plugin.as_str(),
        route.id.as_str()
    )
}

pub(super) fn extension_features_for_disco(state: &WebSocketState) -> Vec<Feature> {
    extension_namespaces_for_disco(state.deps.protocol.extension_manager.extension_features())
}

fn extension_namespaces_for_disco(namespaces: Vec<String>) -> Vec<Feature> {
    namespaces.into_iter().map(|ns| Feature::new(&ns)).collect()
}

pub(super) fn extension_route_metadata_form(
    route: &waddle_extensions::ExtensionRouteDescriptor,
) -> Element {
    use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType, IntoElement};

    DataForm::new(FormType::Result)
        .add_field(Field::form_type(EXTENSION_ROUTE_FORM_TYPE))
        .add_field(Field::text_single(
            "waddle#plugin_id",
            route.plugin.as_str(),
        ))
        .add_field(Field::text_single("waddle#route_id", route.id.as_str()))
        .add_field(Field::text_single(
            "waddle#route_label",
            route.label.as_str(),
        ))
        .add_field(Field::text_single(
            "waddle#route_scope",
            route.scope.as_str(),
        ))
        .add_field(Field::text_single(
            "waddle#route_surface",
            route.surface.as_str(),
        ))
        .add_field(Field::text_single(
            "waddle#state_node",
            route.state_node.as_str(),
        ))
        .add_field(Field::text_single(
            "waddle#payload_ns",
            route.payload_namespace.as_str(),
        ))
        .into_element()
}

pub(super) fn extension_command_metadata_form(
    plugin: &waddle_extensions::PluginId,
    descriptor: &waddle_extensions::CommandDescriptor,
) -> Element {
    use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType, IntoElement};

    DataForm::new(FormType::Result)
        .add_field(Field::form_type(EXTENSION_COMMAND_FORM_TYPE))
        .add_field(Field::text_single("waddle#plugin_id", plugin.as_str()))
        .add_field(Field::text_single(
            "waddle#command_node",
            descriptor.node.as_str(),
        ))
        .add_field(Field::text_single(
            "waddle#command_label",
            descriptor.name.as_str(),
        ))
        .add_field(Field::text_single(
            "waddle#command_scope",
            descriptor.scope.as_str(),
        ))
        .into_element()
}
