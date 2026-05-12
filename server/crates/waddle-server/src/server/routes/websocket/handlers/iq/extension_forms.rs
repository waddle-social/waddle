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
    profile: Option<&waddle_extensions::ExtensionProfile>,
) -> Element {
    use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType, IntoElement};

    add_profile_fields(
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
            )),
        profile,
    )
    .into_element()
}

pub(super) fn extension_command_metadata_form(
    plugin: &waddle_extensions::PluginId,
    descriptor: &waddle_extensions::CommandDescriptor,
    profile: Option<&waddle_extensions::ExtensionProfile>,
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
    form = add_profile_fields(form, profile);
    form.into_element()
}

fn add_profile_fields(
    mut form: waddle_xmpp::xep::xep0004::DataForm,
    profile: Option<&waddle_extensions::ExtensionProfile>,
) -> waddle_xmpp::xep::xep0004::DataForm {
    use waddle_xmpp::xep::xep0004::Field;

    let Some(profile) = profile else {
        return form;
    };

    form = form.add_field(Field::text_single(
        "waddle#profile_display_name",
        profile.display_name.as_str(),
    ));
    if let Some(description) = profile.description.as_ref() {
        form = form.add_field(Field::text_single(
            "waddle#profile_description",
            description.as_str(),
        ));
    }
    if let Some(accent) = profile.accent.as_deref() {
        form = form.add_field(Field::text_single("waddle#profile_accent", accent));
    }
    if let Some(avatar) = profile.avatar.as_ref() {
        form = form.add_field(Field::text_single(
            "waddle#profile_avatar_uri",
            avatar.uri.as_str(),
        ));
    }
    if let Some(label) = profile.bot_hat_label.as_ref() {
        form = form.add_field(Field::text_single("waddle#bot_hat_label", label.as_str()));
    }
    form
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

    fn profile() -> waddle_extensions::ExtensionProfile {
        waddle_extensions::ExtensionProfile {
            display_name: waddle_extensions::DisplayText::new("GitHub").expect("display name"),
            description: Some(
                waddle_extensions::DisplayText::new("Repository events")
                    .expect("profile description"),
            ),
            accent: Some("#238636".to_string()),
            avatar: None,
            bot_hat_label: Some(waddle_extensions::DisplayText::new("Bot").expect("hat label")),
        }
    }

    fn route() -> waddle_extensions::ExtensionRouteDescriptor {
        waddle_extensions::ExtensionRouteDescriptor {
            plugin: plugin("github"),
            id: waddle_extensions::RouteId::new("routes").expect("route id"),
            label: waddle_extensions::DisplayText::new("GitHub Routes").expect("route label"),
            scope: waddle_extensions::ExtensionRouteScope::Channel,
            surface: waddle_extensions::ExtensionRouteSurface::List,
            state_node: waddle_extensions::PubSubNode::new(
                "urn:waddle:web-integration:1:github:routes",
            )
            .expect("state node"),
            payload_namespace: waddle_extensions::PayloadNamespace::new(
                "urn:waddle:web-integration:1",
            )
            .expect("payload namespace"),
        }
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
            None,
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
            None,
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
            None,
        );
        assert_eq!(
            field_value(&form, "waddle#composer_prefix").as_deref(),
            Some("poll"),
        );
        assert!(!has_field(&form, "waddle#inline_field"));
    }

    #[test]
    fn command_metadata_form_includes_extension_profile_fields() {
        let extension_profile = profile();
        let form = extension_command_metadata_form(
            &plugin("github"),
            &descriptor(
                "urn:waddle:extension:1:github",
                waddle_extensions::CommandScope::Channel,
                Some("github"),
                None,
            ),
            Some(&extension_profile),
        );

        assert_eq!(
            field_value(&form, "waddle#profile_display_name").as_deref(),
            Some("GitHub"),
        );
        assert_eq!(
            field_value(&form, "waddle#profile_description").as_deref(),
            Some("Repository events"),
        );
        assert_eq!(
            field_value(&form, "waddle#profile_accent").as_deref(),
            Some("#238636"),
        );
        assert_eq!(
            field_value(&form, "waddle#bot_hat_label").as_deref(),
            Some("Bot"),
        );
    }

    #[test]
    fn route_metadata_form_includes_extension_profile_fields() {
        let extension_profile = profile();
        let form = extension_route_metadata_form(&route(), Some(&extension_profile));

        assert_eq!(
            field_value(&form, "waddle#profile_display_name").as_deref(),
            Some("GitHub"),
        );
        assert_eq!(
            field_value(&form, "waddle#profile_description").as_deref(),
            Some("Repository events"),
        );
        assert_eq!(
            field_value(&form, "waddle#profile_accent").as_deref(),
            Some("#238636"),
        );
        assert_eq!(
            field_value(&form, "waddle#bot_hat_label").as_deref(),
            Some("Bot"),
        );
    }
}
