use super::*;

pub(super) fn saved_link_extension_item(
    url: &str,
    normalized: &str,
    saved_at: &str,
) -> types::ExtensionPayload {
    let title = link_display_title(url);
    let mut item = ExtensionItem::new();
    item.with_title(&title);
    item.with_link(url);
    item.with_field("saved-at", "Saved", saved_at);
    if normalized != url {
        item.with_field("normalized-url", "Normalized URL", normalized);
    }
    item.into_payload()
}

pub(super) fn link_display_title(url: &str) -> String {
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        let host = rest.split('/').next().unwrap_or(rest);
        if !host.is_empty() {
            return host.to_string();
        }
    }
    url.to_string()
}

pub(super) fn launch(
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

pub(super) fn view(
    id_value: &str,
    title: &str,
    text: &str,
    launch_id_value: &str,
) -> types::UiView {
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

pub(super) fn link_payload(
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

pub(super) fn payload(
    root: &str,
    attrs: Vec<(&str, String)>,
    text: &str,
) -> types::ExtensionPayload {
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

/// Builder for the generic `<extension-item xmlns="urn:waddle:extension:1">`
/// envelope that every Waddle extension publishes for its PubSub state items.
///
/// The host renders these items uniformly regardless of which extension
/// produced them, so the inner element vocabulary is fixed by the framework
/// (title/subtitle/link/description/field/option/action).
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

    fn with_link(&mut self, href: &str) -> &mut Self {
        self.push_empty_element("link", vec![("href", href.to_string())]);
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

pub(super) fn framework_xml_element(
    local_name: &str,
    attrs: Vec<(&str, String)>,
) -> types::XmlElement {
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

pub(super) fn framework_namespace() -> types::PayloadNamespace {
    types::PayloadNamespace {
        value: FRAMEWORK_NS.to_string(),
    }
}
