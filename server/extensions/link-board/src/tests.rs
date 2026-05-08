use super::*;

#[test]
fn manifest_declares_link_board_commands_as_channel_scoped() {
    let manifest = manifest();
    assert!(!manifest.commands.is_empty());
    for command in &manifest.commands {
        assert!(
            matches!(command.scope, types::CommandScope::Channel),
            "command {} should require an active channel context",
            command.node.value,
        );
    }
}

fn xml_element_token(token: &types::XmlToken) -> &types::XmlElement {
    match token {
        types::XmlToken::StartElement(element) => element,
        _ => panic!("expected start-element token"),
    }
}

#[test]
fn saved_link_publishes_extension_item_envelope() {
    let payload = saved_link_extension_item(
        "https://example.org/post",
        "https://example.org/post",
        "2026-04-27T00:00:00Z",
    );

    assert_eq!(payload.namespace.value, FRAMEWORK_NS);
    assert_eq!(payload.root.namespace.value, FRAMEWORK_NS);
    assert_eq!(payload.root.local_name, EXTENSION_ITEM_ROOT);

    let root = xml_element_token(&payload.tokens[0]);
    assert_eq!(root.namespace.value, FRAMEWORK_NS);
    assert_eq!(root.local_name, EXTENSION_ITEM_ROOT);

    // Walk the children to confirm the envelope contents.
    let mut titles: Vec<String> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    let mut field_names: Vec<String> = Vec::new();
    let mut depth = 0;
    let mut current_local: Option<String> = None;
    let mut current_attrs: Vec<types::XmlAttribute> = Vec::new();
    let mut current_text = String::new();
    for token in payload.tokens.iter().skip(1) {
        match token {
            types::XmlToken::StartElement(element) => {
                depth += 1;
                if depth == 1 {
                    current_local = Some(element.local_name.clone());
                    current_attrs = element.attributes.clone();
                    current_text.clear();
                }
            }
            types::XmlToken::Text(text) if depth == 1 => current_text.push_str(text),
            types::XmlToken::Text(_) => {}
            types::XmlToken::EndElement => {
                if depth == 1 {
                    if let Some(local) = current_local.take() {
                        match local.as_str() {
                            "title" => titles.push(current_text.clone()),
                            "link" => {
                                if let Some(href) =
                                    current_attrs.iter().find(|attr| attr.local_name == "href")
                                {
                                    links.push(href.value.clone());
                                }
                            }
                            "field" => {
                                if let Some(name) =
                                    current_attrs.iter().find(|attr| attr.local_name == "name")
                                {
                                    field_names.push(name.value.clone());
                                }
                            }
                            _ => {}
                        }
                    }
                    current_attrs.clear();
                    current_text.clear();
                }
                depth -= 1;
            }
        }
    }
    assert_eq!(titles, vec!["example.org".to_string()]);
    assert_eq!(links, vec!["https://example.org/post".to_string()]);
    assert_eq!(field_names, vec!["saved-at".to_string()]);
}

#[test]
fn link_display_title_falls_back_to_url() {
    assert_eq!(
        link_display_title("https://example.org/path"),
        "example.org"
    );
    assert_eq!(link_display_title("not-a-url"), "not-a-url");
}
