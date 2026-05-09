mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "waddle-extension",
        with: {
            "wasi:logging/logging@0.1.0-draft": generate,
            "wasi:clocks/monotonic-clock@0.2.0": generate,
            "wasi:io/poll@0.2.0": generate,
        },
    });
}

use bindings::exports;
#[cfg(not(test))]
use bindings::waddle::extension::host_tools;
#[cfg(not(test))]
use bindings::waddle::extension::runtime;
use bindings::waddle::extension::types;
use serde::Deserialize;

struct GitHub;

bindings::export!(GitHub with_types_in bindings);

const PLUGIN_ID: &str = "github";
const PLUGIN_NAME: &str = "GitHub";
const PLUGIN_NS: &str = "urn:waddle:web-integration:1";
const VERSION: &str = "0.1.0";

impl exports::waddle::extension::lifecycle::Guest for GitHub {
    fn init(config: String) -> Result<types::ExtensionManifest, String> {
        parse_config(&config)
            .map_err(|error| format!("github configuration is invalid: {error}"))?;
        Ok(manifest())
    }
}

impl exports::waddle::extension::framework::Guest for GitHub {
    fn handle_event(
        event: types::ExtensionEvent,
    ) -> Result<types::ExtensionResponse, types::ExtensionError> {
        let effects = match event {
            types::ExtensionEvent::ProviderWebhook(webhook) => {
                handle_provider_webhook(webhook, current_config()?)?
            }
            types::ExtensionEvent::MessageHook(_)
            | types::ExtensionEvent::Command(_)
            | types::ExtensionEvent::Launch(_) => vec![],
        };
        Ok(types::ExtensionResponse { effects })
    }
}

fn manifest() -> types::ExtensionManifest {
    types::ExtensionManifest {
        id: plugin_id(),
        name: display(PLUGIN_NAME),
        version: types::PluginVersion {
            value: VERSION.to_string(),
        },
        payloads: vec![payload_rule(
            types::PayloadSurface::PubsubItem,
            "github-event",
        )],
        capabilities: vec![types::ExtensionCapability::HostMessageSend],
        commands: vec![],
        routes: vec![],
        pubsub_nodes: vec![],
        artifact: None,
    }
}

fn handle_provider_webhook(
    webhook: types::ProviderWebhook,
    config: GitHubConfig,
) -> Result<Vec<types::ExtensionEffect>, types::ExtensionError> {
    if webhook.provider.value != "github" {
        return Ok(vec![]);
    }
    let Some(alert) = alert_for_webhook(&webhook) else {
        return Ok(vec![]);
    };
    let routes = config.routes_for(&webhook);
    if routes.is_empty() {
        return Ok(vec![]);
    }
    for route in routes {
        send_room_message(&route.channel, display(alert.body.as_str()))?;
    }
    Ok(vec![types::ExtensionEffect::Noop])
}

fn alert_for_webhook(webhook: &types::ProviderWebhook) -> Option<GitHubAlert> {
    let payload = GitHubPayload::from_webhook(webhook)?;
    if payload.action != "completed" {
        return None;
    }
    let conclusion = payload.conclusion.as_str();
    if !matches!(conclusion, "failure" | "timed_out" | "cancelled") {
        return None;
    }
    if !matches!(payload.event_type.as_str(), "workflow_run" | "check_run") {
        return None;
    }

    let name = payload.name.as_str();
    let repository = payload.repository_full_name.as_str();
    let mut body = format!("GitHub {repository}: {name} completed with {conclusion}");
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitHubAlert {
    body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitHubPayload {
    event_type: String,
    action: String,
    installation_id: String,
    repository_id: String,
    repository_full_name: String,
    conclusion: String,
    name: String,
    branch: Option<String>,
    revision: Option<String>,
    url: Option<String>,
}

impl GitHubPayload {
    fn from_webhook(webhook: &types::ProviderWebhook) -> Option<Self> {
        if webhook.provider.value != "github" {
            return None;
        }
        let event_type = webhook.event_type.value.as_str();
        let prefix = match event_type {
            "workflow_run" => "workflow_run",
            "check_run" => "check_run",
            _ => return None,
        };
        let fields = ProviderFields::new(&webhook.payload);
        Some(Self {
            event_type: event_type.to_string(),
            action: fields.text(&["action"])?.to_string(),
            installation_id: fields.text(&["installation", "id"])?.to_string(),
            repository_id: fields.text(&["repository", "id"])?.to_string(),
            repository_full_name: fields.text(&["repository", "full_name"])?.to_string(),
            conclusion: fields.text(&[prefix, "conclusion"])?.to_string(),
            name: fields.text(&[prefix, "name"])?.to_string(),
            branch: fields
                .text(&[prefix, "head_branch"])
                .or_else(|| fields.text(&[prefix, "check_suite", "head_branch"]))
                .map(str::to_string),
            revision: fields.text(&[prefix, "head_sha"]).map(str::to_string),
            url: fields.text(&[prefix, "html_url"]).map(str::to_string),
        })
    }
}

struct ProviderFields<'a> {
    payload: &'a types::ProviderPayload,
}

impl<'a> ProviderFields<'a> {
    fn new(payload: &'a types::ProviderPayload) -> Self {
        Self { payload }
    }

    fn text(&self, path: &[&str]) -> Option<&'a str> {
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
}

#[derive(Debug, Clone)]
struct GitHubConfig {
    routes: Vec<RouteConfig>,
}

impl GitHubConfig {
    fn routes_for(&self, webhook: &types::ProviderWebhook) -> Vec<RouteConfig> {
        self.routes
            .iter()
            .filter(|route| route.matches(webhook))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone)]
struct RouteConfig {
    installation_id: Option<String>,
    repository_id: String,
    channel: types::RoomJid,
    events: Vec<String>,
}

impl RouteConfig {
    fn matches(&self, webhook: &types::ProviderWebhook) -> bool {
        GitHubPayload::from_webhook(webhook).is_some_and(|payload| {
            self.installation_id
                .as_ref()
                .map(|installation_id| installation_id == &payload.installation_id)
                .unwrap_or(true)
                && self.repository_id == payload.repository_id
                && (self.events.is_empty()
                    || self.events.iter().any(|event| event == &payload.event_type))
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    routes: Vec<RawRoute>,
}

#[derive(Debug, Deserialize)]
struct RawRoute {
    #[serde(default, alias = "installationId")]
    installation_id: Option<String>,
    #[serde(alias = "repositoryId")]
    repository_id: String,
    channel: String,
    #[serde(default)]
    events: Vec<String>,
}

fn parse_config(raw: &str) -> Result<GitHubConfig, String> {
    let raw = if raw.trim().is_empty() { "{}" } else { raw };
    let parsed = serde_json::from_str::<RawConfig>(raw).map_err(|error| error.to_string())?;
    let routes = parsed
        .routes
        .into_iter()
        .map(|route| {
            if route
                .installation_id
                .as_ref()
                .is_some_and(|installation_id| installation_id.trim().is_empty())
            {
                return Err("route installation_id must not be empty".to_string());
            }
            if route.repository_id.trim().is_empty() {
                return Err("route repository_id must not be empty".to_string());
            }
            if route.channel.trim().is_empty() {
                return Err("route channel must not be empty".to_string());
            }
            Ok(RouteConfig {
                installation_id: route.installation_id,
                repository_id: route.repository_id,
                channel: types::RoomJid {
                    value: route.channel,
                },
                events: route.events,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(GitHubConfig { routes })
}

fn current_config() -> Result<GitHubConfig, types::ExtensionError> {
    #[cfg(not(test))]
    {
        parse_config(&runtime::get_config()).map_err(|error| {
            extension_error(
                types::ExtensionErrorCode::InvalidRequest,
                &format!("github configuration is invalid: {error}"),
            )
        })
    }
    #[cfg(test)]
    parse_config("{}").map_err(|error| {
        extension_error(
            types::ExtensionErrorCode::InvalidRequest,
            &format!("github configuration is invalid: {error}"),
        )
    })
}

fn send_room_message(
    room: &types::RoomJid,
    body: types::DisplayText,
) -> Result<(), types::ExtensionError> {
    let request = types::SendMessageRequest {
        target: types::MessageTarget::Muc(room.clone()),
        body,
        thread_id: None,
        reply_to: None,
        extensions: None,
    };
    send_message_request(&request)
}

#[cfg(not(test))]
fn send_message_request(request: &types::SendMessageRequest) -> Result<(), types::ExtensionError> {
    host_tools::send_message(request)
        .map(|_| ())
        .map_err(extension_error_from_host_tool)
}

#[cfg(test)]
fn send_message_request(request: &types::SendMessageRequest) -> Result<(), types::ExtensionError> {
    sent_room_messages()
        .lock()
        .expect("sent room messages lock")
        .push(request.clone());
    Ok(())
}

#[cfg(test)]
fn sent_room_messages() -> &'static std::sync::Mutex<Vec<types::SendMessageRequest>> {
    static SENT_ROOM_MESSAGES: std::sync::OnceLock<
        std::sync::Mutex<Vec<types::SendMessageRequest>>,
    > = std::sync::OnceLock::new();
    SENT_ROOM_MESSAGES.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

fn extension_error(code: types::ExtensionErrorCode, message: &str) -> types::ExtensionError {
    types::ExtensionError {
        code,
        message: display(message),
    }
}

#[cfg(not(test))]
fn extension_error_from_host_tool(error: types::HostToolError) -> types::ExtensionError {
    let code = match error.code {
        types::HostToolErrorCode::Denied => types::ExtensionErrorCode::Denied,
        types::HostToolErrorCode::InvalidRequest => types::ExtensionErrorCode::InvalidRequest,
        types::HostToolErrorCode::NotFound
        | types::HostToolErrorCode::Unsupported
        | types::HostToolErrorCode::TemporaryFailure => types::ExtensionErrorCode::TemporaryFailure,
    };
    extension_error(code, error.message.value.as_str())
}

fn payload_rule(surface: types::PayloadSurface, root: &str) -> types::PayloadRule {
    types::PayloadRule {
        surface,
        root: types::PayloadRoot {
            namespace: payload_namespace(),
            local_name: root.to_string(),
        },
    }
}

fn plugin_id() -> types::PluginId {
    types::PluginId {
        value: PLUGIN_ID.to_string(),
    }
}

fn payload_namespace() -> types::PayloadNamespace {
    types::PayloadNamespace {
        value: PLUGIN_NS.to_string(),
    }
}

fn display(value: &str) -> types::DisplayText {
    types::DisplayText {
        value: value.to_string(),
    }
}

#[cfg(test)]
mod tests;
