//! Implementation behind the parent module facade.

use super::bindings;
use super::exports;
#[cfg(not(test))]
use super::host_tools;
#[cfg(not(test))]
use super::runtime;
use super::types;
use super::Deserialize;

pub(crate) struct GitHub;

pub(crate) const PLUGIN_ID: &str = "github";
pub(crate) const PLUGIN_NAME: &str = "GitHub";
pub(crate) const PLUGIN_NS: &str = "urn:waddle:web-integration:1";
pub(crate) const VERSION: &str = "0.1.0";
pub(crate) const ROUTES_NODE: &str = "urn:waddle:web-integration:1:github:routes";
pub(crate) const ROUTE_ELEMENT: &str = "github-route";
pub(crate) const COMMAND_NODE: &str = "urn:waddle:extension:1:github";
pub(crate) const CONFIGURE_ROUTE_FORM_TYPE: &str =
    "urn:waddle:web-integration:1:github:configure-route";

pub(crate) const FIELD_FORM_TYPE: &str = "FORM_TYPE";
pub(crate) const FIELD_REPOSITORY_ID: &str = "repository_id";
pub(crate) const FIELD_CHANNEL: &str = "channel";
pub(crate) const FIELD_EVENTS: &str = "events";
pub(crate) const FIELD_INSTALLATION_ID: &str = "installation_id";

pub(crate) const ATTR_REPOSITORY_ID: &str = "repository-id";
pub(crate) const ATTR_CHANNEL: &str = "channel";
pub(crate) const ATTR_EVENTS: &str = "events";
pub(crate) const ATTR_INSTALLATION_ID: &str = "installation-id";
pub(crate) const LEGACY_ALERT_ROOM: &str = "chat@muc.waddle.social";
pub(crate) const GITHUB_ACTIONS_ALERT_ROOM: &str = "github-actions@muc.waddle.social";

impl exports::waddle::extension::lifecycle::Guest for GitHub {
    fn init(config: String) -> Result<types::ExtensionManifest, String> {
        super::parse_config(&config)
            .map_err(|error| format!("github configuration is invalid: {error}"))?;
        Ok(super::manifest())
    }
}

impl exports::waddle::extension::framework::Guest for GitHub {
    fn handle_event(
        event: types::ExtensionEvent,
        _delivery: Option<&bindings::waddle::extension::delivery::DeliveryKey>,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::RoomMessageObserve(_) => vec![],
            types::ExtensionEvent::ProviderWebhook(webhook) => {
                super::handle_provider_webhook(webhook)?
            }
            types::ExtensionEvent::Command(command) => {
                super::handle_command(command, super::current_config()?)?
            }
            types::ExtensionEvent::MessageHook(_) | types::ExtensionEvent::Launch(_) => vec![],
        };
        Ok(types::ExtensionResponse {
            effects,
            usage: None,
        })
    }
}

pub(crate) fn manifest() -> types::ExtensionManifest {
    types::ExtensionManifest {
        id: super::plugin_id(),
        name: super::display(super::PLUGIN_NAME),
        version: types::PluginVersion {
            value: super::VERSION.to_string(),
        },
        payloads: vec![
            super::payload_rule(types::PayloadSurface::MessageEnrichment, "github-event"),
            payload_rule(types::PayloadSurface::PubsubItem, "github-event"),
            payload_rule(types::PayloadSurface::PubsubItem, ROUTE_ELEMENT),
        ],
        capabilities: vec![
            types::ExtensionCapability::MessageEnrich,
            types::ExtensionCapability::HostMessageSend,
            types::ExtensionCapability::Commands,
            types::ExtensionCapability::PubsubPublish,
        ],
        commands: vec![super::command_descriptor(
            COMMAND_NODE,
            "Configure GitHub repository alert route",
        )],
        routes: vec![],
        pubsub_nodes: vec![types::PubsubNode {
            value: ROUTES_NODE.to_string(),
        }],
        profile: Some(types::ExtensionProfile {
            display_name: display(PLUGIN_NAME),
            description: Some(display("GitHub webhook alerts")),
            accent: Some("green".to_string()),
            avatar: None,
            bot_hat_label: Some(display("Bot")),
        }),
        artifact: None,
    }
}

pub(crate) fn command_descriptor(node: &str, name: &str) -> types::CommandDescriptor {
    types::CommandDescriptor {
        node: types::CommandNode {
            value: node.to_string(),
        },
        name: display(name),
        scope: types::CommandScope::Global,
        composer_prefix: None,
        inline_field: None,
        composer_execute: false,
    }
}

// ---------------------------------------------------------------------------
// Provider webhook dispatch
// ---------------------------------------------------------------------------

pub(crate) fn handle_provider_webhook(
    webhook: types::ProviderWebhook,
) -> Result<Vec<types::ExtensionEffect>, types::ExtensionError> {
    if webhook.provider.value != "github" {
        return Ok(vec![]);
    }
    let Some(payload) = super::GitHubPayload::from_webhook(&webhook) else {
        return Ok(vec![]);
    };
    let Some(alert) = super::alert_for_payload(&payload) else {
        return Ok(vec![]);
    };
    let routes = super::load_routes()?;
    let matching: Vec<_> = routes
        .iter()
        .filter(|route| route.matches(&payload))
        .collect();
    if matching.is_empty() {
        return Ok(vec![]);
    }
    let mut effects = vec![];
    for route in matching {
        let route = super::migrate_route(route);
        if route.migrated {
            effects.push(types::ExtensionEffect::PublishPubsub(
                route.current.to_publish_effect(),
            ));
        }
        super::send_room_message(
            &types::RoomJid {
                value: route.current.channel.clone(),
            },
            display(alert.body.as_str()),
            Some(super::github_envelope(&webhook, &payload, &alert)),
        )?;
    }
    effects.push(types::ExtensionEffect::Noop);
    Ok(effects)
}

pub(crate) fn alert_for_payload(payload: &GitHubPayload) -> Option<super::GitHubAlert> {
    if !super::should_alert_for_payload(payload) {
        return None;
    }
    let name = payload.name.as_str();
    let repository = payload.repository_full_name.as_str();
    let mut body = if payload.is_status_event() {
        format!(
            "GitHub {repository}: {name} {} with {}",
            payload.action, payload.conclusion
        )
    } else {
        format!(
            "GitHub {repository}: {} {}",
            payload.event_type, payload.action
        )
    };
    if let Some(branch) = payload.branch.as_ref() {
        body.push_str(" on ");
        body.push_str(branch.as_str());
    }
    if let Some(revision) = payload.revision.as_ref() {
        let short = revision.get(0..7).unwrap_or(revision.as_str());
        body.push_str(" at ");
        body.push_str(short);
    }
    if let Some(url) = payload.url.as_ref() {
        body.push('\n');
        body.push_str(url.as_str());
    }
    Some(GitHubAlert { body })
}

pub(crate) fn should_alert_for_payload(payload: &GitHubPayload) -> bool {
    match payload.event_type.as_str() {
        "workflow_run" | "check_run" => {
            payload.action == "completed"
                && super::is_workflow_check_alert_conclusion(payload.conclusion.as_str())
        }
        "deployment_status" => super::is_deployment_alert_state(payload.conclusion.as_str()),
        _ => false,
    }
}

pub(crate) fn is_workflow_check_alert_conclusion(conclusion: &str) -> bool {
    conclusion == "failure"
}

pub(crate) fn is_deployment_alert_state(state: &str) -> bool {
    state == "failure"
}

pub(crate) fn github_envelope(
    webhook: &types::ProviderWebhook,
    payload: &GitHubPayload,
    alert: &GitHubAlert,
) -> types::ExtensionEnvelope {
    types::ExtensionEnvelope {
        version: 1,
        enrichments: vec![types::MessageEnrichment {
            id: types::EnrichmentId {
                value: format!("github-{}", webhook.delivery_id.value),
            },
            plugin: plugin_id(),
            capability: types::ExtensionCapability::MessageEnrich,
            payload_namespace: payload_namespace(),
            created_at: super::timestamp(),
            source: None,
            ui: vec![types::UiView {
                id: types::UiViewId {
                    value: "github-event".to_string(),
                },
                title: Some(display("GitHub")),
                blocks: vec![types::UiBlock::Text(types::TextBlock {
                    text: display(alert.body.as_str()),
                    style: types::TextStyle::Body,
                })],
            }],
            payloads: vec![payload.to_enrichment_payload()],
            launches: vec![],
        }],
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitHubAlert {
    pub(crate) body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitHubPayload {
    pub(crate) event_type: String,
    pub(crate) action: String,
    pub(crate) installation_id: String,
    pub(crate) repository_id: String,
    pub(crate) repository_full_name: String,
    pub(crate) conclusion: String,
    pub(crate) name: String,
    pub(crate) branch: Option<String>,
    pub(crate) revision: Option<String>,
    pub(crate) url: Option<String>,
}

impl GitHubPayload {
    pub(crate) fn from_webhook(webhook: &types::ProviderWebhook) -> Option<Self> {
        if webhook.provider.value != "github" {
            return None;
        }
        let event_type = webhook.event_type.value.as_str();
        let prefix = super::status_payload_prefix(event_type);
        let fields = super::ProviderFields::new(&webhook.payload);
        Some(Self {
            event_type: event_type.to_string(),
            action: fields.text(&["action"])?.to_string(),
            installation_id: fields.text(&["installation", "id"])?.to_string(),
            repository_id: fields.text(&["repository", "id"])?.to_string(),
            repository_full_name: fields.text(&["repository", "full_name"])?.to_string(),
            conclusion: super::status_conclusion(&fields, event_type, prefix)
                .unwrap_or("unknown")
                .to_string(),
            name: super::status_name(&fields, event_type, prefix)
                .unwrap_or(event_type)
                .to_string(),
            branch: super::status_branch(&fields, event_type, prefix).map(str::to_string),
            revision: super::status_revision(&fields, event_type, prefix).map(str::to_string),
            url: super::status_url(&fields, event_type, prefix).map(str::to_string),
        })
    }

    pub(crate) fn is_status_event(&self) -> bool {
        status_payload_prefix(self.event_type.as_str()).is_some()
    }

    pub(crate) fn to_enrichment_payload(&self) -> types::ExtensionPayload {
        // `surface` declares what kind of UI the payload expects the
        // client to render. GitHub events are server-pushed
        // notifications, not human replies, so we render them as a
        // chat-bot surface (full-width system band with no author
        // gutter). The framework keeps the attribute optional so
        // existing payloads default to the standard message-card
        // treatment; extensions that ARE bot-style declare themselves.
        let mut attributes = vec![
            xml_attr("surface", "chat-bot"),
            xml_attr("event-type", &self.event_type),
            xml_attr("action", &self.action),
            xml_attr("installation-id", &self.installation_id),
            xml_attr("repository-id", &self.repository_id),
            xml_attr("repository", &self.repository_full_name),
            xml_attr("conclusion", &self.conclusion),
            xml_attr("name", &self.name),
        ];
        if let Some(branch) = self.branch.as_ref() {
            attributes.push(super::xml_attr("branch", branch));
        }
        if let Some(revision) = self.revision.as_ref() {
            attributes.push(xml_attr("revision", revision));
        }
        if let Some(url) = self.url.as_ref() {
            attributes.push(xml_attr("url", url));
        }
        types::ExtensionPayload {
            namespace: super::payload_namespace(),
            root: types::PayloadRoot {
                namespace: payload_namespace(),
                local_name: "github-event".to_string(),
            },
            tokens: vec![
                types::XmlToken::StartElement(types::XmlElement {
                    namespace: payload_namespace(),
                    local_name: "github-event".to_string(),
                    attributes,
                }),
                types::XmlToken::EndElement,
            ],
        }
    }
}

pub(crate) fn status_payload_prefix(event_type: &str) -> Option<&'static str> {
    match event_type {
        "workflow_run" => Some("workflow_run"),
        "check_run" => Some("check_run"),
        "deployment_status" => Some("deployment_status"),
        _ => None,
    }
}

pub(crate) fn status_conclusion<'a>(
    fields: &ProviderFields<'a>,
    event_type: &str,
    prefix: Option<&str>,
) -> Option<&'a str> {
    match event_type {
        "deployment_status" => fields.non_empty_text(&["deployment_status", "state"]),
        _ => prefix.and_then(|prefix| fields.non_empty_text(&[prefix, "conclusion"])),
    }
}

pub(crate) fn status_name<'a>(
    fields: &ProviderFields<'a>,
    event_type: &str,
    prefix: Option<&str>,
) -> Option<&'a str> {
    match event_type {
        "deployment_status" => fields
            .non_empty_text(&["deployment_status", "environment"])
            .or_else(|| fields.non_empty_text(&["deployment", "environment"])),
        _ => prefix.and_then(|prefix| fields.non_empty_text(&[prefix, "name"])),
    }
}

pub(crate) fn status_branch<'a>(
    fields: &ProviderFields<'a>,
    event_type: &str,
    prefix: Option<&str>,
) -> Option<&'a str> {
    match event_type {
        "deployment_status" => fields.non_empty_text(&["deployment", "ref"]),
        _ => prefix.and_then(|prefix| {
            fields
                .non_empty_text(&[prefix, "head_branch"])
                .or_else(|| fields.non_empty_text(&[prefix, "check_suite", "head_branch"]))
        }),
    }
}

pub(crate) fn status_revision<'a>(
    fields: &ProviderFields<'a>,
    event_type: &str,
    prefix: Option<&str>,
) -> Option<&'a str> {
    match event_type {
        "deployment_status" => fields
            .non_empty_text(&["deployment", "sha"])
            .or_else(|| fields.non_empty_text(&["deployment_status", "deployment", "sha"])),
        _ => prefix.and_then(|prefix| fields.non_empty_text(&[prefix, "head_sha"])),
    }
}

pub(crate) fn status_url<'a>(
    fields: &ProviderFields<'a>,
    event_type: &str,
    prefix: Option<&str>,
) -> Option<&'a str> {
    match event_type {
        "deployment_status" => fields
            .non_empty_text(&["deployment_status", "target_url"])
            .or_else(|| fields.non_empty_text(&["deployment_status", "log_url"]))
            .or_else(|| fields.non_empty_text(&["deployment_status", "environment_url"]))
            .or_else(|| fields.non_empty_text(&["repository", "html_url"])),
        _ => prefix.and_then(|prefix| fields.non_empty_text(&[prefix, "html_url"])),
    }
}

pub(crate) struct ProviderFields<'a> {
    pub(crate) payload: &'a types::ProviderPayload,
}

impl<'a> ProviderFields<'a> {
    pub(crate) fn new(payload: &'a types::ProviderPayload) -> Self {
        Self { payload }
    }

    pub(crate) fn text(&self, path: &[&str]) -> Option<&'a str> {
        self.payload
            .fields
            .iter()
            .find(|field| {
                field.path.len() == path.len()
                    && field
                        .path
                        .iter()
                        .zip(path.iter())
                        .all(|(left, right)| left.value == *right)
            })
            .and_then(|field| match &field.value {
                types::ProviderFieldValue::Text(value) => Some(value.value.as_str()),
                types::ProviderFieldValue::Number(value) => Some(value.value.as_str()),
                _ => None,
            })
    }

    pub(crate) fn non_empty_text(&self, path: &[&str]) -> Option<&'a str> {
        self.text(path).filter(|value| !value.trim().is_empty())
    }
}

// ---------------------------------------------------------------------------
// Route persistence via PubSub
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Route {
    pub(crate) repository_id: String,
    pub(crate) channel: String,
    pub(crate) events: Vec<String>,
    pub(crate) installation_id: Option<String>,
}

impl super::Route {
    pub(crate) fn matches(&self, payload: &GitHubPayload) -> bool {
        if self.repository_id != payload.repository_id {
            return false;
        }
        if let Some(installation) = self.installation_id.as_ref() {
            if installation != &payload.installation_id {
                return false;
            }
        }
        self.events.is_empty() || self.events.iter().any(|event| event == &payload.event_type)
    }

    pub(crate) fn to_payload(&self) -> types::ExtensionPayload {
        let mut attributes = vec![
            types::XmlAttribute {
                namespace: None,
                local_name: ATTR_REPOSITORY_ID.to_string(),
                value: self.repository_id.clone(),
            },
            types::XmlAttribute {
                namespace: None,
                local_name: ATTR_CHANNEL.to_string(),
                value: self.channel.clone(),
            },
            types::XmlAttribute {
                namespace: None,
                local_name: ATTR_EVENTS.to_string(),
                value: self.events.join(","),
            },
        ];
        if let Some(installation_id) = self.installation_id.as_ref() {
            attributes.push(types::XmlAttribute {
                namespace: None,
                local_name: super::ATTR_INSTALLATION_ID.to_string(),
                value: installation_id.clone(),
            });
        }
        types::ExtensionPayload {
            namespace: payload_namespace(),
            root: types::PayloadRoot {
                namespace: payload_namespace(),
                local_name: super::ROUTE_ELEMENT.to_string(),
            },
            tokens: vec![
                types::XmlToken::StartElement(types::XmlElement {
                    namespace: payload_namespace(),
                    local_name: ROUTE_ELEMENT.to_string(),
                    attributes,
                }),
                types::XmlToken::EndElement,
            ],
        }
    }

    pub(crate) fn to_publish_effect(&self) -> types::PubsubPublish {
        types::PubsubPublish {
            node: types::PubsubNode {
                value: super::ROUTES_NODE.to_string(),
            },
            item_id: Some(types::PubsubItemId {
                value: self.repository_id.clone(),
            }),
            payload: self.to_payload(),
        }
    }

    pub(crate) fn from_payload(payload: &types::ExtensionPayload) -> Option<Self> {
        if payload.root.namespace.value != super::PLUGIN_NS
            || payload.root.local_name != ROUTE_ELEMENT
        {
            return None;
        }
        let start = payload.tokens.iter().find_map(|token| match token {
            types::XmlToken::StartElement(element) if element.local_name == ROUTE_ELEMENT => {
                Some(element)
            }
            _ => None,
        })?;
        let repository_id =
            super::find_attr(&start.attributes, super::ATTR_REPOSITORY_ID)?.to_string();
        let channel = find_attr(&start.attributes, super::ATTR_CHANNEL)?.to_string();
        let events = find_attr(&start.attributes, super::ATTR_EVENTS)
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|event| !event.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let installation_id =
            find_attr(&start.attributes, ATTR_INSTALLATION_ID).map(str::to_string);
        Some(Self {
            repository_id,
            channel,
            events,
            installation_id,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MigratedRoute {
    pub(crate) current: Route,
    pub(crate) migrated: bool,
}

pub(crate) fn migrate_route(route: &Route) -> super::MigratedRoute {
    if route.channel == super::LEGACY_ALERT_ROOM {
        let mut current = route.clone();
        current.channel = super::GITHUB_ACTIONS_ALERT_ROOM.to_string();
        return MigratedRoute {
            current,
            migrated: true,
        };
    }
    MigratedRoute {
        current: route.clone(),
        migrated: false,
    }
}

pub(crate) fn find_attr<'a>(attrs: &'a [types::XmlAttribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|attr| attr.local_name == name)
        .map(|attr| attr.value.as_str())
}

#[cfg(not(test))]
pub(crate) fn load_routes() -> Result<Vec<Route>, types::ExtensionError> {
    let request = types::PubsubGetItemsRequest {
        node: types::PubsubNode {
            value: ROUTES_NODE.to_string(),
        },
        max_items: None,
        item_ids: vec![],
    };
    let response =
        host_tools::pubsub_get_items(&request).map_err(super::extension_error_from_host_tool)?;
    Ok(response
        .items
        .into_iter()
        .filter_map(|item| Route::from_payload(&item.payload))
        .collect())
}

// ---------------------------------------------------------------------------
// XEP-0050 admin command: configure GitHub alert route
// ---------------------------------------------------------------------------

pub(crate) fn handle_command(
    command: types::CommandInvocation,
    config: super::GitHubConfig,
) -> Result<Vec<types::ExtensionEffect>, types::ExtensionError> {
    if command.command_node.value != super::COMMAND_NODE {
        return Ok(vec![]);
    }
    let requester_bare = super::bare_jid_value(&command.requester.value);
    if !config.admins.iter().any(|admin| admin == &requester_bare) {
        return Err(super::extension_error(
            types::ExtensionErrorCode::Denied,
            &format!("{requester_bare} is not authorized to configure github routes"),
        ));
    }
    if matches!(command.action, Some(types::CommandAction::Cancel)) {
        return Ok(vec![]);
    }
    let repository_id = super::field_value(&command.fields, super::FIELD_REPOSITORY_ID);
    let channel = field_value(&command.fields, super::FIELD_CHANNEL);
    if repository_id.is_none() || channel.is_none() {
        return Ok(vec![types::ExtensionEffect::CommandForm(
            super::configure_route_form(),
        )]);
    }
    super::require_form_type(&command.fields, super::CONFIGURE_ROUTE_FORM_TYPE)?;
    let route = migrate_route(&super::build_route_from_fields(&command.fields)?).current;
    Ok(vec![types::ExtensionEffect::PublishPubsub(
        route.to_publish_effect(),
    )])
}

pub(crate) fn configure_route_form() -> types::DataForm {
    types::DataForm {
        form_type: types::DataFormType::Form,
        title: Some(display("Configure GitHub route")),
        instructions: vec![display(
            "Map a GitHub repository's workflow/check failures to a MUC room.",
        )],
        fields: vec![
            types::DataFormField {
                name: types::UiActionId {
                    value: FIELD_FORM_TYPE.to_string(),
                },
                field_type: types::FormFieldType::Hidden,
                label: None,
                required: false,
                values: vec![form_value(CONFIGURE_ROUTE_FORM_TYPE)],
                options: vec![],
            },
            types::DataFormField {
                name: types::UiActionId {
                    value: FIELD_REPOSITORY_ID.to_string(),
                },
                field_type: types::FormFieldType::TextSingle,
                label: Some(display(
                    "Repository ID (numeric, from `gh api /repos/<owner>/<repo> --jq .id`)",
                )),
                required: true,
                values: vec![],
                options: vec![],
            },
            types::DataFormField {
                name: types::UiActionId {
                    value: FIELD_CHANNEL.to_string(),
                },
                field_type: types::FormFieldType::JidSingle,
                label: Some(display(
                    "Destination room JID (must be in the provider room grant list)",
                )),
                required: true,
                values: vec![],
                options: vec![],
            },
            types::DataFormField {
                name: types::UiActionId {
                    value: FIELD_EVENTS.to_string(),
                },
                field_type: types::FormFieldType::ListMulti,
                label: Some(display("Event types")),
                required: true,
                values: vec![
                    form_value("workflow_run"),
                    form_value("check_run"),
                    form_value("deployment_status"),
                ],
                options: vec![
                    super::form_option("Workflow runs", "workflow_run"),
                    form_option("Check runs", "check_run"),
                    form_option("Deployment statuses", "deployment_status"),
                ],
            },
            types::DataFormField {
                name: types::UiActionId {
                    value: FIELD_INSTALLATION_ID.to_string(),
                },
                field_type: types::FormFieldType::TextSingle,
                label: Some(display(
                    "Installation ID (optional; filters to a specific GitHub App installation)",
                )),
                required: false,
                values: vec![],
                options: vec![],
            },
        ],
    }
}

pub(crate) fn build_route_from_fields(
    fields: &[types::FormFieldValue],
) -> Result<Route, types::ExtensionError> {
    let repository_id = super::require_numeric_field(fields, FIELD_REPOSITORY_ID)?;
    let channel = super::require_field(fields, FIELD_CHANNEL)?;
    let events: Vec<String> = super::field_values(fields, super::FIELD_EVENTS)
        .into_iter()
        .filter(|event| !event.trim().is_empty())
        .collect();
    if events.is_empty() {
        return Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "at least one event type must be selected",
        ));
    }
    let installation_id =
        field_value(fields, super::FIELD_INSTALLATION_ID).filter(|value| !value.trim().is_empty());
    Ok(Route {
        repository_id,
        channel,
        events,
        installation_id,
    })
}

pub(crate) fn require_numeric_field(
    fields: &[types::FormFieldValue],
    name: &str,
) -> Result<String, types::ExtensionError> {
    let value = require_field(fields, name)?;
    if value.parse::<u64>().is_err() {
        return Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            &format!("{name} must be a positive integer"),
        ));
    }
    Ok(value)
}

pub(crate) fn require_form_type(
    fields: &[types::FormFieldValue],
    expected: &str,
) -> Result<(), types::ExtensionError> {
    match field_value(fields, super::FIELD_FORM_TYPE).as_deref() {
        Some(actual) if actual == expected => Ok(()),
        Some(_) => Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "submitted form type does not match the GitHub route configuration form",
        )),
        None => Err(extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            "missing required field FORM_TYPE",
        )),
    }
}

pub(crate) fn require_field(
    fields: &[types::FormFieldValue],
    name: &str,
) -> Result<String, types::ExtensionError> {
    field_value(fields, name).ok_or_else(|| {
        extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            &format!("missing required field {name}"),
        )
    })
}

pub(crate) fn field_value(fields: &[types::FormFieldValue], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|field| field.name.value == name)
        .and_then(|field| field.values.first())
        .map(|value| value.value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn field_values(fields: &[types::FormFieldValue], name: &str) -> Vec<String> {
    fields
        .iter()
        .find(|field| field.name.value == name)
        .map(|field| {
            field
                .values
                .iter()
                .map(|value| value.value.trim().to_string())
                .filter(|value| !value.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn form_value(value: &str) -> types::DataFormValue {
    types::DataFormValue {
        value: value.to_string(),
    }
}

pub(crate) fn form_option(label: &str, value: &str) -> types::FormFieldOption {
    types::FormFieldOption {
        label: Some(display(label)),
        value: super::form_value(value),
    }
}

pub(crate) fn bare_jid_value(full_jid: &str) -> String {
    full_jid
        .split_once('/')
        .map(|(bare, _)| bare.to_string())
        .unwrap_or_else(|| full_jid.to_string())
}

// ---------------------------------------------------------------------------
// Config: just admin allow-list now; routes live in PubSub
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct GitHubConfig {
    pub(crate) admins: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct RawConfig {
    pub(crate) admins: Vec<String>,
}

pub(crate) fn parse_config(raw: &str) -> Result<GitHubConfig, String> {
    let raw = if raw.trim().is_empty() { "{}" } else { raw };
    let parsed =
        serde_json::from_str::<super::RawConfig>(raw).map_err(|error| error.to_string())?;
    let admins = parsed
        .admins
        .into_iter()
        .map(|admin| admin.trim().to_string())
        .filter(|admin| !admin.is_empty())
        .collect();
    Ok(GitHubConfig { admins })
}

#[cfg(not(test))]
pub(crate) fn current_config() -> Result<GitHubConfig, types::ExtensionError> {
    parse_config(&runtime::get_config()).map_err(|error| {
        extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            &format!("github configuration is invalid: {error}"),
        )
    })
}

// ---------------------------------------------------------------------------
// Host-tool plumbing
// ---------------------------------------------------------------------------

pub(crate) fn send_room_message(
    room: &types::RoomJid,
    body: types::DisplayText,
    extensions: Option<types::ExtensionEnvelope>,
) -> Result<(), types::ExtensionError> {
    let request = types::SendMessageRequest {
        target: types::MessageTarget::Muc(room.clone()),
        body,
        thread_id: None,
        reply_to: None,
        markup: vec![],
        extensions,
    };
    super::send_message_request(&request)
}

#[cfg(not(test))]
pub(crate) fn send_message_request(
    request: &types::SendMessageRequest,
) -> Result<(), types::ExtensionError> {
    host_tools::send_message(request)
        .map(|_| ())
        .map_err(extension_error_from_host_tool)
}

pub(crate) fn extension_error(
    code: types::ExtensionErrorCode,
    message: &str,
) -> types::ExtensionError {
    types::ExtensionError {
        code,
        message: display(message),
    }
}

#[cfg(not(test))]
pub(crate) fn extension_error_from_host_tool(error: types::HostToolError) -> types::ExtensionError {
    let code = match error.code {
        types::HostToolErrorCode::Denied => types::ExtensionErrorCode::Denied,
        types::HostToolErrorCode::InvalidRequest => types::ExtensionErrorCode::InvalidRequest,
        types::HostToolErrorCode::NotFound
        | types::HostToolErrorCode::Unsupported
        | types::HostToolErrorCode::TemporaryFailure => types::ExtensionErrorCode::TemporaryFailure,
    };
    extension_error(code, error.message.value.as_str())
}

pub(crate) fn payload_rule(surface: types::PayloadSurface, root: &str) -> types::PayloadRule {
    types::PayloadRule {
        surface,
        root: types::PayloadRoot {
            namespace: payload_namespace(),
            local_name: root.to_string(),
        },
    }
}

pub(crate) fn xml_attr(local_name: &str, value: &str) -> types::XmlAttribute {
    types::XmlAttribute {
        namespace: None,
        local_name: local_name.to_string(),
        value: value.to_string(),
    }
}

pub(crate) fn plugin_id() -> types::PluginId {
    types::PluginId {
        value: super::PLUGIN_ID.to_string(),
    }
}

pub(crate) fn payload_namespace() -> types::PayloadNamespace {
    types::PayloadNamespace {
        value: PLUGIN_NS.to_string(),
    }
}

pub(crate) fn display(value: &str) -> types::DisplayText {
    types::DisplayText {
        value: value.to_string(),
    }
}

pub(crate) fn timestamp() -> types::Timestamp {
    types::Timestamp {
        value: super::current_timestamp_value(),
    }
}

#[cfg(not(test))]
pub(crate) fn current_timestamp_value() -> String {
    runtime::current_timestamp()
}
