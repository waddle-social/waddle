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

pub(super) fn extension_namespaces_for_disco(namespaces: Vec<String>) -> Vec<Feature> {
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

    let mut form = DataForm::new(FormType::Result)
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
        ));
    if let Some(prefix) = descriptor.composer_prefix.as_deref() {
        form = form.add_field(Field::text_single("waddle#composer_prefix", prefix));
    }
    if let Some(field_name) = descriptor.inline_field.as_deref() {
        form = form.add_field(Field::text_single("waddle#inline_field", field_name));
    }
    form.into_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(
        node: &str,
        scope: waddle_extensions::CommandScope,
        composer_prefix: Option<&str>,
        inline_field: Option<&str>,
    ) -> waddle_extensions::CommandDescriptor {
        waddle_extensions::CommandDescriptor {
            node: waddle_extensions::CommandNode::new(node).expect("command node"),
            name: waddle_extensions::DisplayText::new("Test Command").expect("display text"),
            scope,
            composer_prefix: composer_prefix.map(str::to_string),
            inline_field: inline_field.map(str::to_string),
        }
    }

    fn plugin(id: &str) -> waddle_extensions::PluginId {
        waddle_extensions::PluginId::new(id).expect("plugin id")
    }

    fn field_value(form: &Element, var: &str) -> Option<String> {
        form.children()
            .filter(|c| c.name() == "field")
            .find_map(|field| {
                (field.attr("var") == Some(var)).then(|| {
                    field
                        .get_child("value", "jabber:x:data")
                        .map(|v| v.text())
                        .unwrap_or_default()
                })
            })
    }

    fn has_field(form: &Element, var: &str) -> bool {
        form.children()
            .filter(|c| c.name() == "field")
            .any(|field| field.attr("var") == Some(var))
    }

    #[test]
    fn command_metadata_form_omits_composer_prefix_when_descriptor_has_none() {
        let form = extension_command_metadata_form(
            &plugin("decision-polls"),
            &descriptor(
                "urn:waddle:extension:1:invoke",
                waddle_extensions::CommandScope::Channel,
                None,
                None,
            ),
        );
        assert!(!has_field(&form, "waddle#composer_prefix"));
        assert!(!has_field(&form, "waddle#inline_field"));
        assert_eq!(
            field_value(&form, "waddle#command_scope").as_deref(),
            Some("channel"),
        );
    }

    #[test]
    fn command_metadata_form_serializes_composer_prefix_and_inline_field() {
        let form = extension_command_metadata_form(
            &plugin("ai-chatbot"),
            &descriptor(
                "urn:waddle:extension:1:ai-chatbot",
                waddle_extensions::CommandScope::Global,
                Some("ai"),
                Some("prompt"),
            ),
        );
        assert_eq!(
            field_value(&form, "waddle#composer_prefix").as_deref(),
            Some("ai"),
        );
        assert_eq!(
            field_value(&form, "waddle#inline_field").as_deref(),
            Some("prompt"),
        );
    }

    #[test]
    fn command_metadata_form_includes_composer_prefix_without_inline_field() {
        let form = extension_command_metadata_form(
            &plugin("decision-polls"),
            &descriptor(
                "urn:waddle:extension:1:decision-polls",
                waddle_extensions::CommandScope::Channel,
                Some("poll"),
                None,
            ),
        );
        assert_eq!(
            field_value(&form, "waddle#composer_prefix").as_deref(),
            Some("poll"),
        );
        assert!(!has_field(&form, "waddle#inline_field"));
    }
}
