use super::*;

pub(super) fn poll_extension_item(poll: &Poll) -> types::ExtensionPayload {
    let mut item = ExtensionItem::new();
    item.with_title(&poll.question);
    item.with_subtitle(&format!("Closes at {}", poll.closes_at));
    for option in &poll.options {
        item.with_option(&option.id, &option.label);
    }
    item.with_action(&format!("vote-{}", poll.poll_id), "Vote");
    item.into_payload()
}

pub(super) fn vote_extension_item(poll_id: &str, option_id: &str) -> types::ExtensionPayload {
    let mut item = ExtensionItem::new();
    item.with_title("Vote recorded");
    item.with_field("poll-id", "Poll", poll_id);
    item.with_field("option-id", "Choice", option_id);
    item.into_payload()
}

pub(super) fn results_extension_item(
    poll_id: &str,
    latest_option_id: &str,
) -> types::ExtensionPayload {
    let mut item = ExtensionItem::new();
    item.with_title(&format!("Results for {poll_id}"));
    item.with_subtitle(&format!("Latest vote: {latest_option_id}"));
    item.with_field("poll-id", "Poll", poll_id);
    item.with_field("latest-option-id", "Latest choice", latest_option_id);
    item.into_payload()
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
