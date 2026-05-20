use super::*;

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";

pub(crate) async fn discover_extension_command_service(
    inner: Rc<RefCell<WaddleClientInner>>,
    domain: &str,
) -> String {
    let fallback_service_jid = format!("extensions.{domain}");
    let mut discovered_items = Vec::new();

    if let Ok(items_result) =
        send_iq_command(inner.clone(), build_disco_items_iq(domain, None)).await
    {
        if let Some(items) = discovery::parse_disco_items_result(&items_result) {
            discovered_items = items;
        }
    }

    for candidate in extension_command_service_candidates(domain, &discovered_items) {
        if let Ok(info_result) =
            send_iq_command(inner.clone(), build_disco_info_iq(&candidate, None)).await
        {
            if let Some(info) = discovery::parse_disco_info_result(&info_result, &candidate) {
                if is_waddle_extension_service(&info) {
                    return candidate;
                }
            }
        }
    }

    fallback_service_jid
}

pub(crate) fn extension_command_service_candidates(
    domain: &str,
    items: &[discovery::DiscoItem],
) -> Vec<String> {
    let fallback_service_jid = format!("extensions.{domain}");
    let mut candidates = Vec::new();

    for item in items {
        push_unique_candidate(&mut candidates, item.jid.as_str());
    }
    push_unique_candidate(&mut candidates, &fallback_service_jid);
    push_unique_candidate(&mut candidates, domain);

    candidates
}

pub(crate) fn push_unique_candidate(candidates: &mut Vec<String>, candidate: &str) {
    if !candidates.iter().any(|existing| existing == candidate) {
        candidates.push(candidate.to_string());
    }
}

pub(crate) fn is_waddle_extension_service(info: &discovery::DiscoInfoResult) -> bool {
    info.has_feature(NS_WADDLE_EXTENSION_1) && info.has_feature(NS_ADHOC_COMMANDS)
}

pub(crate) fn parse_extension_routes_from_info(
    info: &discovery::DiscoInfoResult,
    service_jid: &str,
) -> Vec<WaddleExtensionRoute> {
    info.forms
        .iter()
        .filter(|form| form.form_type.as_deref() == Some(EXTENSION_ROUTE_FORM_TYPE))
        .filter_map(|form| {
            let plugin_id = disco_form_value(form, "waddle#plugin_id")?;
            let route_id = disco_form_value(form, "waddle#route_id")?;
            let label = disco_form_value(form, "waddle#route_label")?;
            let scope = disco_form_value(form, "waddle#route_scope")?;
            let surface = disco_form_value(form, "waddle#route_surface")?;
            let state_node = disco_form_value(form, "waddle#state_node")?;
            let payload_namespace = disco_form_value(form, "waddle#payload_ns")?;
            if scope != "channel" || (surface != "gallery" && surface != "list") {
                return None;
            }
            Some(WaddleExtensionRoute {
                service_jid: service_jid.to_string(),
                plugin_id: plugin_id.to_string(),
                route_id: route_id.to_string(),
                label: label.to_string(),
                scope: scope.to_string(),
                surface: surface.to_string(),
                state_node: state_node.to_string(),
                payload_namespace: payload_namespace.to_string(),
            })
        })
        .collect()
}

pub(crate) fn disco_form_value<'a>(
    form: &'a discovery::DiscoDataForm,
    var: &str,
) -> Option<&'a str> {
    form.fields
        .iter()
        .find(|field| field.var == var)
        .and_then(|field| field.values.first())
        .map(String::as_str)
}

/// Build an XEP-0060 pubsub `<items>` IQ directed at `to` for `node`.
pub(crate) fn build_extension_route_items_iq(
    to: &str,
    node: &str,
    max_items: Option<u32>,
) -> Element {
    let id = format!("pubsub-items-{}", uuid::Uuid::new_v4());
    let mut items_builder = Element::builder("items", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    let max_items_value;
    if let Some(max_items) = max_items {
        max_items_value = max_items.to_string();
        items_builder = items_builder.attr(
            minidom::rxml::xml_ncname!("max_items").to_owned(),
            max_items_value.as_str(),
        );
    }
    let items = items_builder.build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB).append(items).build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .append(pubsub)
        .build()
}

pub(crate) fn parse_extension_route_items_result(iq: &Element) -> Vec<WaddleExtensionRouteItem> {
    let Some(pubsub) = iq.get_child("pubsub", NS_PUBSUB) else {
        return Vec::new();
    };
    let Some(items) = pubsub.get_child("items", NS_PUBSUB) else {
        return Vec::new();
    };
    items
        .children()
        .filter(|el| el.name() == "item" && el.ns() == NS_PUBSUB)
        .filter_map(parse_extension_route_item)
        .collect()
}

pub(crate) fn parse_extension_route_item(item: &Element) -> Option<WaddleExtensionRouteItem> {
    let payload = item
        .children()
        .find(|child| child.name() == "extension-item" && child.ns() == NS_WADDLE_EXTENSION_1)?;
    let mut view = WaddleExtensionRouteItem {
        id: item.attr("id").map(str::to_string),
        title: None,
        subtitle: None,
        link: None,
        description: None,
        fields: Vec::new(),
        options: Vec::new(),
        actions: Vec::new(),
    };

    for child in payload
        .children()
        .filter(|child| child.ns() == NS_WADDLE_EXTENSION_1)
    {
        match child.name() {
            "title" => {
                let value = child.text().trim().to_string();
                if !value.is_empty() {
                    view.title = Some(value);
                }
            }
            "subtitle" => {
                let value = child.text().trim().to_string();
                if !value.is_empty() {
                    view.subtitle = Some(value);
                }
            }
            "description" => {
                let value = child.text().trim().to_string();
                if !value.is_empty() {
                    view.description = Some(value);
                }
            }
            "link" => {
                if let Some(href) = child.attr("href").filter(|href| !href.trim().is_empty()) {
                    view.link = Some(WaddleExtensionRouteLink {
                        href: href.to_string(),
                    });
                }
            }
            "field" => {
                if let Some(name) = child.attr("name").filter(|name| !name.trim().is_empty()) {
                    view.fields.push(WaddleExtensionRouteItemField {
                        name: name.to_string(),
                        label: child
                            .attr("label")
                            .filter(|label| !label.trim().is_empty())
                            .map(str::to_string),
                        value: child.text().trim().to_string(),
                    });
                }
            }
            "option" => {
                if let (Some(id), Some(label)) = (child.attr("id"), child.attr("label")) {
                    if !id.trim().is_empty() && !label.trim().is_empty() {
                        view.options.push(WaddleExtensionRouteItemOption {
                            id: id.to_string(),
                            label: label.to_string(),
                        });
                    }
                }
            }
            "action" => {
                if let (
                    Some(launch_id),
                    Some(label),
                    Some(plugin_id),
                    Some(action_id),
                    Some(command_node),
                    Some(launch_token),
                    Some(expires_at),
                    Some(waddle_id),
                ) = (
                    child
                        .attr("launch-id")
                        .filter(|value| !value.trim().is_empty()),
                    child.attr("label").filter(|value| !value.trim().is_empty()),
                    child
                        .attr("plugin")
                        .filter(|value| !value.trim().is_empty()),
                    child
                        .attr("action")
                        .filter(|value| !value.trim().is_empty()),
                    child
                        .attr("command-node")
                        .filter(|value| !value.trim().is_empty()),
                    child.attr("token").filter(|value| !value.trim().is_empty()),
                    child
                        .attr("expires-at")
                        .filter(|value| !value.trim().is_empty()),
                    child
                        .attr("waddle-id")
                        .filter(|value| !value.trim().is_empty()),
                ) {
                    view.actions.push(WaddleExtensionRouteItemAction {
                        launch: WaddleExtensionRouteItemLaunch {
                            id: launch_id.to_string(),
                            plugin_id: plugin_id.to_string(),
                            action_id: action_id.to_string(),
                            command_node: command_node.to_string(),
                            label: label.to_string(),
                            launch_token: launch_token.to_string(),
                            expires_at: expires_at.to_string(),
                            waddle_id: waddle_id.to_string(),
                            room_jid: child.attr("room").map(str::to_string),
                            source_stanza_id: child.attr("source-stanza-id").map(str::to_string),
                        },
                    });
                }
            }
            _ => {}
        }
    }

    Some(view)
}

#[cfg(test)]
mod extension_route_tests {
    use super::*;

    #[test]
    fn extension_route_items_iq_uses_xep_0060_items_with_limit() {
        let iq = build_extension_route_items_iq(
            "extensions.example.com",
            "urn:waddle:link-board:1:channel:general@muc.example.com:links",
            Some(100),
        );

        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.ns(), NS_CLIENT);
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("to"), Some("extensions.example.com"));
        let pubsub = iq.get_child("pubsub", NS_PUBSUB).expect("pubsub");
        let items = pubsub.get_child("items", NS_PUBSUB).expect("items");
        assert_eq!(
            items.attr("node"),
            Some("urn:waddle:link-board:1:channel:general@muc.example.com:links")
        );
        assert_eq!(items.attr("max_items"), Some("100"));
    }

    #[test]
    fn parses_xep_0128_route_forms_from_disco_info() {
        let info = discovery::DiscoInfoResult {
            jid: "extensions.example.com".to_string(),
            node: Some("urn:waddle:link-board:1:channel:{room}:links".to_string()),
            identities: Vec::new(),
            features: Vec::new(),
            forms: vec![discovery::DiscoDataForm {
                form_type: Some(EXTENSION_ROUTE_FORM_TYPE.to_string()),
                fields: vec![
                    field("FORM_TYPE", EXTENSION_ROUTE_FORM_TYPE),
                    field("waddle#plugin_id", "link-board"),
                    field("waddle#route_id", "saved-links"),
                    field("waddle#route_label", "Saved Links"),
                    field("waddle#route_scope", "channel"),
                    field("waddle#route_surface", "gallery"),
                    field(
                        "waddle#state_node",
                        "urn:waddle:link-board:1:channel:{room}:links",
                    ),
                    field("waddle#payload_ns", "urn:waddle:link-board:1"),
                ],
            }],
        };

        let routes = parse_extension_routes_from_info(&info, "extensions.example.com");

        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].service_jid, "extensions.example.com");
        assert_eq!(routes[0].plugin_id, "link-board");
        assert_eq!(routes[0].route_id, "saved-links");
        assert_eq!(routes[0].surface, "gallery");
        assert_eq!(
            routes[0].state_node,
            "urn:waddle:link-board:1:channel:{room}:links"
        );
    }

    #[test]
    fn extension_service_candidates_probe_advertised_items_before_domain_fallbacks() {
        let items = vec![
            disco_item("muc.alt.example.com"),
            disco_item("upload.waddle.social"),
            disco_item("extensions.alt.example.com"),
        ];

        let candidates = extension_command_service_candidates("waddle.social", &items);

        assert_eq!(
            candidates,
            vec![
                "muc.alt.example.com",
                "upload.waddle.social",
                "extensions.alt.example.com",
                "extensions.waddle.social",
                "waddle.social",
            ]
        );
    }

    #[test]
    fn waddle_extension_service_requires_framework_and_commands_features() {
        assert!(!is_waddle_extension_service(&disco_info(&[
            NS_ADHOC_COMMANDS
        ])));
        assert!(!is_waddle_extension_service(&disco_info(&[
            NS_WADDLE_EXTENSION_1
        ])));
        assert!(is_waddle_extension_service(&disco_info(&[
            NS_WADDLE_EXTENSION_1,
            NS_ADHOC_COMMANDS,
        ])));
    }

    #[test]
    fn parses_extension_item_envelopes_from_pubsub_items() {
        let iq = Element::builder("iq", NS_CLIENT)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(
                Element::builder("pubsub", NS_PUBSUB)
                    .append(
                        Element::builder("items", NS_PUBSUB)
                            .attr(
                                minidom::rxml::xml_ncname!("node").to_owned(),
                                "urn:waddle:decision-polls:1:channel:general:polls",
                            )
                            .append(
                                Element::builder("item", NS_PUBSUB)
                                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "poll-42")
                                    .append(
                                        Element::builder("extension-item", NS_WADDLE_EXTENSION_1)
                                            .append(
                                                Element::builder("title", NS_WADDLE_EXTENSION_1)
                                                    .append("Lunch tomorrow?")
                                                    .build(),
                                            )
                                            .append(
                                                Element::builder("subtitle", NS_WADDLE_EXTENSION_1)
                                                    .append("Open")
                                                    .build(),
                                            )
                                            .append(
                                                Element::builder("link", NS_WADDLE_EXTENSION_1)
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("href")
                                                            .to_owned(),
                                                        "https://example.org/poll",
                                                    )
                                                    .build(),
                                            )
                                            .append(
                                                Element::builder("field", NS_WADDLE_EXTENSION_1)
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("name")
                                                            .to_owned(),
                                                        "saved-at",
                                                    )
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("label")
                                                            .to_owned(),
                                                        "Saved",
                                                    )
                                                    .append("2026-04-27T00:00:00Z")
                                                    .build(),
                                            )
                                            .append(
                                                Element::builder("option", NS_WADDLE_EXTENSION_1)
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("id").to_owned(),
                                                        "a",
                                                    )
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("label")
                                                            .to_owned(),
                                                        "Pizza",
                                                    )
                                                    .build(),
                                            )
                                            .append(
                                                Element::builder("action", NS_WADDLE_EXTENSION_1)
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("launch-id")
                                                            .to_owned(),
                                                        "vote-42",
                                                    )
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("label")
                                                            .to_owned(),
                                                        "Vote",
                                                    )
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("plugin")
                                                            .to_owned(),
                                                        "decision-polls",
                                                    )
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("action")
                                                            .to_owned(),
                                                        "vote",
                                                    )
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("command-node")
                                                            .to_owned(),
                                                        "urn:waddle:extensions:invoke",
                                                    )
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("token")
                                                            .to_owned(),
                                                        "signed-token",
                                                    )
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("expires-at")
                                                            .to_owned(),
                                                        "2026-04-27T00:00:00Z",
                                                    )
                                                    .attr(
                                                        minidom::rxml::xml_ncname!("waddle-id")
                                                            .to_owned(),
                                                        "alice@example.com",
                                                    )
                                                    .build(),
                                            )
                                            .build(),
                                    )
                                    .build(),
                            )
                            .append(
                                Element::builder("item", NS_PUBSUB)
                                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "raw")
                                    .append(
                                        Element::builder("saved-item", "urn:waddle:unknown:1")
                                            .build(),
                                    )
                                    .build(),
                            )
                            .build(),
                    )
                    .build(),
            )
            .build();

        let items = parse_extension_route_items_result(&iq);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.as_deref(), Some("poll-42"));
        assert_eq!(items[0].title.as_deref(), Some("Lunch tomorrow?"));
        assert_eq!(items[0].subtitle.as_deref(), Some("Open"));
        assert_eq!(
            items[0].link.as_ref().map(|link| link.href.as_str()),
            Some("https://example.org/poll")
        );
        assert_eq!(items[0].fields[0].name, "saved-at");
        assert_eq!(items[0].fields[0].label.as_deref(), Some("Saved"));
        assert_eq!(items[0].options[0].id, "a");
        assert_eq!(items[0].actions[0].launch.id, "vote-42");
        assert_eq!(items[0].actions[0].launch.launch_token, "signed-token");
    }

    fn field(var: &str, value: &str) -> discovery::DiscoDataField {
        discovery::DiscoDataField {
            var: var.to_string(),
            values: vec![value.to_string()],
        }
    }

    fn disco_item(jid: &str) -> discovery::DiscoItem {
        discovery::DiscoItem {
            jid: jid.to_string(),
            name: None,
            node: None,
        }
    }

    fn disco_info(features: &[&str]) -> discovery::DiscoInfoResult {
        discovery::DiscoInfoResult {
            jid: "example.com".to_string(),
            node: None,
            identities: Vec::new(),
            features: features.iter().map(|feature| feature.to_string()).collect(),
            forms: Vec::new(),
        }
    }
}
