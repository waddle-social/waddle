use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use directories::{BaseDirs, ProjectDirs};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use xmpp_parsers::{disco, iq::Iq, minidom::Element};

use waddle_core::config::{self, Config};
use waddle_core::event::{
    BroadcastEventBus, Channel, ChatMessage, Event, EventBus, EventPayload, EventSource,
    PresenceShow, RosterItem, ScrollDirection, UiTarget,
};
use waddle_mam::MamManager;
use waddle_messaging::{MessageManager, MucManager};
use waddle_notifications::NotificationManager;
use waddle_plugins::{
    InstalledPlugin, PluginCapability, PluginError, PluginInfo as RuntimePluginInfo,
    PluginRegistry, PluginRuntime, PluginRuntimeConfig, PluginStatus as RuntimePluginStatus,
    RegistryConfig, RegistryError,
};
use waddle_presence::PresenceManager;
use waddle_roster::RosterManager;
use waddle_storage::{self, NativeDatabase, StorageError};
use waddle_xmpp::{
    ChatStateProcessor, ConnectionConfig, ConnectionManager, ConnectionState, MamProcessor,
    MessageProcessor, MucProcessor, OutboundRouter, PresenceProcessor, RosterProcessor, Stanza,
    StanzaPipeline, parse_stanza, stanza_channel,
};

#[cfg(debug_assertions)]
use waddle_xmpp::DebugProcessor;

const SYSTEM_COMPONENT: &str = "gui-backend";
const CONNECTION_TIMEOUT_SECONDS: u32 = 30;
const CONNECTION_MAX_RECONNECT_ATTEMPTS: u32 = 5;
const WIRE_CHANNEL_CAPACITY: usize = 256;
const SHUTDOWN_CLEANUP_TIMEOUT_SECONDS: u64 = 5;
const MUC_OWNER_SUBMIT_QUERY_XML: &str = "<query xmlns='http://jabber.org/protocol/muc#owner'><x xmlns='jabber:x:data' type='submit'/></query>";
const MUC_OWNER_DESTROY_QUERY_XML: &str =
    "<query xmlns='http://jabber.org/protocol/muc#owner'><destroy/></query>";
static NEXT_IQ_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, thiserror::Error)]
enum GuiBackendError {
    #[error("configuration error: {0}")]
    Config(#[from] config::ConfigError),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("event bus error: {0}")]
    EventBus(#[from] waddle_core::error::EventBusError),

    #[error("plugin registry error: {0}")]
    PluginRegistry(#[from] RegistryError),

    #[error("plugin runtime error: {0}")]
    PluginRuntime(#[from] PluginError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("command failed: {command}: {reason}")]
    CommandFailed { command: String, reason: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UiConfigResponse {
    notifications: bool,
    theme: String,
    locale: Option<String>,
    theme_name: String,
    custom_theme_path: Option<String>,
}

impl UiConfigResponse {
    fn from_config(config: &Config) -> Self {
        Self {
            notifications: config.ui.notifications,
            theme: config.ui.theme.clone(),
            locale: config.ui.locale.clone(),
            theme_name: config.theme.name.clone(),
            custom_theme_path: config.theme.custom_path.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginInfoResponse {
    id: String,
    name: String,
    version: String,
    status: String,
    error_reason: Option<String>,
    error_count: u32,
    capabilities: Vec<String>,
}

impl PluginInfoResponse {
    fn from_runtime(runtime_info: RuntimePluginInfo) -> Self {
        let (status, error_reason) = match runtime_info.status {
            RuntimePluginStatus::Loading => ("loading".to_string(), None),
            RuntimePluginStatus::Active => ("active".to_string(), None),
            RuntimePluginStatus::Error(reason) => ("error".to_string(), Some(reason)),
            RuntimePluginStatus::Unloading => ("unloading".to_string(), None),
        };

        let capabilities = runtime_info
            .capabilities
            .iter()
            .map(capability_label)
            .collect();

        Self {
            id: runtime_info.id,
            name: runtime_info.name,
            version: runtime_info.version,
            status,
            error_reason,
            error_count: runtime_info.error_count,
            capabilities,
        }
    }

    fn from_installed(installed: InstalledPlugin, status: &str) -> Self {
        Self {
            id: installed.id,
            name: installed.name,
            version: installed.version,
            status: status.to_string(),
            error_reason: None,
            error_count: 0,
            capabilities: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionStateResponse {
    status: String,
    jid: Option<String>,
    attempt: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RoomInfoResponse {
    jid: String,
    name: String,
}

impl ConnectionStateResponse {
    fn from_connection_state(state: ConnectionState, own_jid: &str) -> Self {
        match state {
            ConnectionState::Connected => Self {
                status: "connected".to_string(),
                jid: Some(own_jid.split('/').next().unwrap_or(own_jid).to_string()),
                attempt: None,
            },
            ConnectionState::Connecting => Self {
                status: "connecting".to_string(),
                jid: None,
                attempt: None,
            },
            ConnectionState::Reconnecting { attempt } => Self {
                status: "reconnecting".to_string(),
                jid: None,
                attempt: Some(attempt),
            },
            ConnectionState::Disconnected => Self {
                status: "offline".to_string(),
                jid: None,
                attempt: None,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
enum PluginAction {
    Install { reference: String },
    Uninstall { plugin_id: String },
    Update { plugin_id: String },
    Get { plugin_id: String },
}

struct AppState {
    own_jid: Arc<Mutex<String>>,
    ui_config: UiConfigResponse,
    event_bus: Arc<dyn EventBus>,
    connection_manager: Arc<Mutex<ConnectionManager>>,
    roster_manager: Arc<RosterManager<NativeDatabase>>,
    message_manager: Arc<MessageManager<NativeDatabase>>,
    muc_manager: Arc<MucManager<NativeDatabase>>,
    mam_manager: Arc<MamManager<NativeDatabase>>,
    presence_manager: Arc<PresenceManager>,
    plugin_registry: Arc<PluginRegistry>,
    plugin_runtime: Arc<Mutex<PluginRuntime<NativeDatabase>>>,
}

#[tauri::command]
async fn send_message(
    to: String,
    body: String,
    state: State<'_, AppState>,
) -> Result<ChatMessage, String> {
    state
        .message_manager
        .send_message(&to, &body)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn connect(
    jid: String,
    password: String,
    endpoint: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let bare_jid = jid.split('/').next().unwrap_or(&jid).trim().to_string();
    let domain = domain_from_jid(&bare_jid).ok_or_else(|| {
        "Invalid JID. Expected format user@domain (resource optional).".to_string()
    })?;

    let (server, port) = match parse_endpoint_server_override(&endpoint) {
        Some((host, parsed_port)) => (Some(host), parsed_port),
        None => (Some(domain), None),
    };

    let new_config = ConnectionConfig {
        jid: bare_jid.clone(),
        password,
        server,
        port,
        timeout_seconds: CONNECTION_TIMEOUT_SECONDS,
        max_reconnect_attempts: CONNECTION_MAX_RECONNECT_ATTEMPTS,
    };

    let current_bare_jid = {
        let own_jid = state.own_jid.lock().await;
        own_jid
            .split('/')
            .next()
            .unwrap_or(own_jid.as_str())
            .trim()
            .to_string()
    };

    let mut connection = state.connection_manager.lock().await;
    if !current_bare_jid.is_empty()
        && current_bare_jid != bare_jid
        && !matches!(connection.state(), ConnectionState::Disconnected)
    {
        return Err(
            "Switching connected JIDs at runtime is not supported. Disconnect first.".to_string(),
        );
    }
    let _ = connection.disconnect().await;
    *connection = ConnectionManager::with_event_bus(new_config, state.event_bus.clone());
    connection
        .connect()
        .await
        .map_err(|error| error.to_string())?;

    let mut own_jid = state.own_jid.lock().await;
    *own_jid = bare_jid;
    Ok(())
}

#[tauri::command]
async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    let mut connection = state.connection_manager.lock().await;
    connection
        .disconnect()
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn get_roster(state: State<'_, AppState>) -> Result<Vec<RosterItem>, String> {
    let mut items = state
        .roster_manager
        .get_roster()
        .await
        .map_err(|error| error.to_string())?;

    // Inject a synthetic entry for the connected user so they always see themselves
    let own_jid = state.own_jid.lock().await.clone();
    let bare_jid = own_jid.split('/').next().unwrap_or(&own_jid);
    let self_already_present = items.iter().any(|item| item.jid == bare_jid);
    if !self_already_present && !bare_jid.is_empty() {
        let localpart = bare_jid.split('@').next().unwrap_or(bare_jid);
        items.insert(
            0,
            RosterItem {
                jid: bare_jid.to_string(),
                name: Some(localpart.to_string()),
                subscription: waddle_core::event::Subscription::Both,
                groups: vec!["Self".to_string()],
            },
        );
    }

    Ok(items)
}

#[tauri::command]
async fn add_contact(jid: String, state: State<'_, AppState>) -> Result<(), String> {
    // Add to local roster storage and send roster-set IQ to the server
    state
        .roster_manager
        .add_contact(&jid, None, &[])
        .await
        .map_err(|error| error.to_string())?;

    // Request a presence subscription so both sides can see each other
    state
        .roster_manager
        .request_subscription(&jid)
        .await
        .map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
async fn get_connection_state(
    state: State<'_, AppState>,
) -> Result<ConnectionStateResponse, String> {
    let own_jid = state.own_jid.lock().await.clone();
    let connection = state.connection_manager.lock().await;
    Ok(ConnectionStateResponse::from_connection_state(
        connection.state(),
        &own_jid,
    ))
}

#[tauri::command]
async fn set_presence(
    show: String,
    status: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let show = parse_presence_show(&show).ok_or_else(|| {
        format!(
            "invalid presence show '{show}'; expected one of: available, chat, away, xa, dnd, unavailable"
        )
    })?;

    state
        .presence_manager
        .set_own_presence(show, status.as_deref(), None)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn join_room(
    room_jid: String,
    nick: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .muc_manager
        .join_room(&room_jid, &nick)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn leave_room(room_jid: String, state: State<'_, AppState>) -> Result<(), String> {
    state
        .muc_manager
        .leave_room(&room_jid)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn discover_muc_service(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let own_jid = state.own_jid.lock().await.clone();
    let Some(domain) = domain_from_jid(&own_jid) else {
        return Ok(None);
    };

    match discover_muc_service_via_disco(state.inner(), &domain).await {
        Ok(service) => Ok(Some(service)),
        Err(_) => Ok(Some(format!("muc.{domain}"))),
    }
}

#[tauri::command]
async fn list_rooms(
    service_jid: String,
    state: State<'_, AppState>,
) -> Result<Vec<RoomInfoResponse>, String> {
    let service = if service_jid.trim().is_empty() {
        let own_jid = state.own_jid.lock().await.clone();
        let domain = domain_from_jid(&own_jid).ok_or_else(|| {
            "Cannot discover MUC service without a valid connected JID".to_string()
        })?;
        discover_muc_service_via_disco(state.inner(), &domain)
            .await
            .unwrap_or_else(|_| format!("muc.{domain}"))
    } else {
        service_jid.trim().to_string()
    };

    let iq_id = next_iq_id();
    let iq = build_disco_items_query_stanza(&iq_id, &service)?;

    let response =
        send_iq_and_wait_for_result(state.inner(), &iq_id, iq, Duration::from_secs(8)).await?;

    let mut rooms = extract_disco_items(&response)
        .into_iter()
        .filter(|(jid, _)| !jid.is_empty())
        .map(|(jid, name)| RoomInfoResponse {
            name: name.unwrap_or_else(|| jid.clone()),
            jid,
        })
        .collect::<Vec<_>>();

    rooms.sort_by(|left, right| left.jid.cmp(&right.jid));
    Ok(rooms)
}

#[tauri::command]
async fn create_room(
    room_jid: String,
    nick: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut joined_subscription = state
        .event_bus
        .subscribe("xmpp.muc.joined")
        .map_err(|error| error.to_string())?;

    state
        .muc_manager
        .join_room(&room_jid, &nick)
        .await
        .map_err(|error| error.to_string())?;

    wait_for_muc_join(&mut joined_subscription, &room_jid, Duration::from_secs(8)).await?;

    // Accept instant-room defaults with an owner config submit (XEP-0045).
    let iq_id = next_iq_id();
    let iq = build_muc_owner_submit_stanza(&iq_id, &room_jid)?;

    send_iq_and_wait_for_result(state.inner(), &iq_id, iq, Duration::from_secs(8))
        .await
        .map(|_| ())
}

#[tauri::command]
async fn delete_room(room_jid: String, state: State<'_, AppState>) -> Result<(), String> {
    let iq_id = next_iq_id();
    let iq = build_muc_owner_destroy_stanza(&iq_id, &room_jid)?;

    send_iq_and_wait_for_result(state.inner(), &iq_id, iq, Duration::from_secs(8)).await?;

    // Best effort local cleanup.
    state
        .muc_manager
        .leave_room(&room_jid)
        .await
        .map_err(|error| error.to_string())
}

fn next_iq_id() -> String {
    let sequence = NEXT_IQ_ID.fetch_add(1, Ordering::Relaxed);
    format!("gui-iq-{sequence}")
}

fn domain_from_jid(jid: &str) -> Option<String> {
    let bare = jid.split('/').next().unwrap_or(jid).trim();
    bare.split('@')
        .nth(1)
        .map(str::trim)
        .filter(|domain| !domain.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_endpoint_server_override(endpoint: &str) -> Option<(String, Option<u16>)> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    if authority.is_empty() {
        return None;
    }

    if let Some((host_part, port_part)) = authority.rsplit_once(':')
        && !host_part.is_empty()
        && !port_part.is_empty()
        && port_part.chars().all(|ch| ch.is_ascii_digit())
        && let Ok(port) = port_part.parse::<u16>()
    {
        return Some((host_part.trim_matches(['[', ']']).to_string(), Some(port)));
    }

    Some((authority.trim_matches(['[', ']']).to_string(), None))
}

fn serialize_iq_stanza(iq: Iq) -> Result<Vec<u8>, String> {
    waddle_xmpp::serialize_stanza(&Stanza::Iq(Box::new(iq))).map_err(|error| error.to_string())
}

fn parse_query_element(xml: &str) -> Result<Element, String> {
    xml.parse::<Element>()
        .map_err(|error| format!("Failed to parse IQ query payload XML: {error}"))
}

fn parse_jid(jid: &str) -> Result<xmpp_parsers::jid::Jid, String> {
    jid.parse()
        .map_err(|_| format!("Invalid JID in IQ command: '{jid}'"))
}

fn build_disco_items_query_stanza(iq_id: &str, to: &str) -> Result<Vec<u8>, String> {
    let to_jid = parse_jid(to)?;
    let query = disco::DiscoItemsQuery {
        node: None,
        rsm: None,
    };
    let iq = Iq::from_get(iq_id.to_string(), query).with_to(to_jid);
    serialize_iq_stanza(iq)
}

fn build_disco_info_query_stanza(iq_id: &str, to: &str) -> Result<Vec<u8>, String> {
    let to_jid = parse_jid(to)?;
    let query = disco::DiscoInfoQuery { node: None };
    let iq = Iq::from_get(iq_id.to_string(), query).with_to(to_jid);
    serialize_iq_stanza(iq)
}

fn build_muc_owner_submit_stanza(iq_id: &str, room_jid: &str) -> Result<Vec<u8>, String> {
    let to_jid = parse_jid(room_jid)?;
    let payload = parse_query_element(MUC_OWNER_SUBMIT_QUERY_XML)?;
    let iq = Iq::Set {
        from: None,
        to: Some(to_jid),
        id: iq_id.to_string(),
        payload,
    };
    serialize_iq_stanza(iq)
}

fn build_muc_owner_destroy_stanza(iq_id: &str, room_jid: &str) -> Result<Vec<u8>, String> {
    let to_jid = parse_jid(room_jid)?;
    let payload = parse_query_element(MUC_OWNER_DESTROY_QUERY_XML)?;
    let iq = Iq::Set {
        from: None,
        to: Some(to_jid),
        id: iq_id.to_string(),
        payload,
    };
    serialize_iq_stanza(iq)
}

fn extract_disco_items(iq: &Iq) -> Vec<(String, Option<String>)> {
    let payload = match iq {
        Iq::Result {
            payload: Some(payload),
            ..
        } => payload,
        _ => return Vec::new(),
    };

    let Ok(result) = disco::DiscoItemsResult::try_from(payload.clone()) else {
        return Vec::new();
    };

    result
        .items
        .into_iter()
        .map(|item| (item.jid.to_string(), item.name))
        .collect()
}

fn is_conference_text_identity(iq: &Iq) -> bool {
    let payload = match iq {
        Iq::Result {
            payload: Some(payload),
            ..
        } => payload,
        _ => return false,
    };

    let Ok(result) = disco::DiscoInfoResult::try_from(payload.clone()) else {
        return false;
    };

    result
        .identities
        .iter()
        .any(|identity| identity.category == "conference" && identity.type_ == "text")
}

async fn send_iq_and_wait_for_result(
    state: &AppState,
    iq_id: &str,
    iq_stanza: Vec<u8>,
    timeout: Duration,
) -> Result<Iq, String> {
    let mut subscription = state
        .event_bus
        .subscribe("xmpp.raw.stanza.received")
        .map_err(|error| error.to_string())?;

    {
        let mut connection = state.connection_manager.lock().await;
        connection
            .send_stanza(&iq_stanza)
            .await
            .map_err(|error| error.to_string())?;
    }

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!("Timed out waiting for IQ result (id={iq_id})"));
        }

        let event = match tokio::time::timeout(remaining, subscription.recv()).await {
            Ok(Ok(event)) => event,
            Ok(Err(error)) => return Err(error.to_string()),
            Err(_) => return Err(format!("Timed out waiting for IQ result (id={iq_id})")),
        };

        let EventPayload::RawStanzaReceived { stanza } = event.payload else {
            continue;
        };

        let Ok(parsed) = parse_stanza(stanza.as_bytes()) else {
            continue;
        };
        let Stanza::Iq(iq) = parsed else {
            continue;
        };
        let iq = *iq;

        if iq.id() != iq_id {
            continue;
        }

        if matches!(&iq, Iq::Result { .. }) {
            return Ok(iq);
        }

        if matches!(&iq, Iq::Error { .. }) {
            return Err(format!("Server returned IQ error for id={iq_id}: {stanza}"));
        }
    }
}

async fn discover_muc_service_via_disco(state: &AppState, domain: &str) -> Result<String, String> {
    let iq_id = next_iq_id();
    let disco_items = build_disco_items_query_stanza(&iq_id, domain)?;

    let response =
        send_iq_and_wait_for_result(state, &iq_id, disco_items, Duration::from_secs(8)).await?;

    for (jid, _) in extract_disco_items(&response) {
        let info_id = next_iq_id();
        let disco_info = build_disco_info_query_stanza(&info_id, &jid)?;

        if let Ok(info_response) =
            send_iq_and_wait_for_result(state, &info_id, disco_info, Duration::from_secs(5)).await
            && is_conference_text_identity(&info_response)
        {
            return Ok(jid);
        }
    }

    Ok(format!("muc.{domain}"))
}

async fn wait_for_muc_join(
    subscription: &mut waddle_core::event::EventSubscription,
    room_jid: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!("Timed out waiting to join room '{room_jid}'"));
        }

        let event = match tokio::time::timeout(remaining, subscription.recv()).await {
            Ok(Ok(event)) => event,
            Ok(Err(error)) => return Err(error.to_string()),
            Err(_) => return Err(format!("Timed out waiting to join room '{room_jid}'")),
        };

        if let EventPayload::MucJoined { room, .. } = event.payload
            && room == room_jid
        {
            return Ok(());
        }
    }
}

async fn load_local_history(
    state: &AppState,
    jid: &str,
    limit: u32,
    before: Option<&str>,
) -> Result<Vec<ChatMessage>, String> {
    let messages = state
        .message_manager
        .get_messages(jid, limit, before)
        .await
        .map_err(|error| error.to_string())?;

    if !messages.is_empty() {
        return Ok(messages);
    }

    state
        .muc_manager
        .get_room_messages(jid, limit, before)
        .await
        .map_err(|error| error.to_string())
}

async fn with_remote_history_fallback<LocalFn, LocalFut, RemoteFn, RemoteFut>(
    mut load_local: LocalFn,
    mut fetch_remote: RemoteFn,
) -> Result<Vec<ChatMessage>, String>
where
    LocalFn: FnMut() -> LocalFut,
    LocalFut: Future<Output = Result<Vec<ChatMessage>, String>>,
    RemoteFn: FnMut() -> RemoteFut,
    RemoteFut: Future<Output = Result<(), String>>,
{
    let local_messages = load_local().await?;
    if !local_messages.is_empty() {
        return Ok(local_messages);
    }

    if let Err(error) = fetch_remote().await {
        warn!(error = %error, "remote history fetch failed, falling back to local history");
    }

    load_local().await
}

#[tauri::command]
async fn get_history(
    jid: String,
    limit: u32,
    before: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<ChatMessage>, String> {
    let direction = if before.is_some() {
        ScrollDirection::Up
    } else {
        ScrollDirection::Bottom
    };

    publish_event(
        &state.event_bus,
        "ui.scroll.requested",
        EventSource::Ui(UiTarget::Gui),
        EventPayload::ScrollRequested {
            jid: jid.clone(),
            direction,
        },
    )
    .map_err(|error| error.to_string())?;

    let normalized_limit = limit.max(1);

    with_remote_history_fallback(
        || load_local_history(state.inner(), &jid, normalized_limit, before.as_deref()),
        || async {
            state
                .mam_manager
                .fetch_history(&jid, Some(""), normalized_limit)
                .await
                .map(|messages| {
                    debug!(jid = %jid, count = messages.len(), "MAM fallback history fetch completed");
                })
                .map_err(|error| error.to_string())
        },
    )
    .await
}

#[tauri::command]
async fn manage_plugins(
    action: PluginAction,
    state: State<'_, AppState>,
) -> Result<PluginInfoResponse, String> {
    let app_state = state.inner();
    let result = match action {
        PluginAction::Install { reference } => install_plugin(app_state, &reference).await,
        PluginAction::Uninstall { plugin_id } => uninstall_plugin(app_state, &plugin_id).await,
        PluginAction::Update { plugin_id } => update_plugin(app_state, &plugin_id).await,
        PluginAction::Get { plugin_id } => get_plugin(app_state, &plugin_id).await,
    };

    result.map_err(|error| error.to_string())
}

#[tauri::command]
async fn get_config(state: State<'_, AppState>) -> Result<UiConfigResponse, String> {
    Ok(state.ui_config.clone())
}

fn main() {
    init_tracing();

    let app = tauri::Builder::default()
        .setup(|app| {
            let state = tauri::async_runtime::block_on(initialize_backend(app.handle().clone()))?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect,
            disconnect,
            send_message,
            get_roster,
            add_contact,
            get_connection_state,
            set_presence,
            join_room,
            leave_room,
            discover_muc_service,
            list_rooms,
            create_room,
            delete_room,
            get_history,
            manage_plugins,
            get_config
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Tauri application");

    app.run(move |app_handle, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            if let Some(state) = app_handle.try_state::<AppState>() {
                let _ = publish_shutdown_requested(&state.event_bus, "application exit requested");
            }
        }
    });
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn initialize_backend(app_handle: AppHandle) -> Result<AppState, GuiBackendError> {
    let config = config::load_config()?;
    let ui_config = UiConfigResponse::from_config(&config);

    let storage_path = resolve_storage_path(&config);
    let database: Arc<NativeDatabase> =
        Arc::new(waddle_storage::open_native_database(storage_path.as_path()).await?);

    info!(path = %storage_path.display(), "storage initialized");

    let event_bus: Arc<dyn EventBus> =
        Arc::new(BroadcastEventBus::new(config.event_bus.channel_capacity));

    publish_event(
        &event_bus,
        "system.config.loaded",
        EventSource::System(SYSTEM_COMPONENT.to_string()),
        EventPayload::ConfigReloaded,
    )?;

    publish_event(
        &event_bus,
        "system.storage.ready",
        EventSource::System(SYSTEM_COMPONENT.to_string()),
        EventPayload::StartupComplete,
    )?;

    let plugin_registry = Arc::new(PluginRegistry::new(
        RegistryConfig::default(),
        resolve_plugin_data_dir(&config),
    )?);

    let plugin_runtime = Arc::new(Mutex::new(PluginRuntime::new(
        PluginRuntimeConfig::default(),
        event_bus.clone(),
        database.clone(),
    )));

    if config.plugins.enabled {
        load_installed_plugins(&plugin_registry, &plugin_runtime, &event_bus).await;
    }

    let roster_manager = Arc::new(RosterManager::new(database.clone(), event_bus.clone()));
    let message_manager = Arc::new(MessageManager::new(database.clone(), event_bus.clone()));
    let muc_manager = Arc::new(MucManager::new(database.clone(), event_bus.clone()));
    let presence_manager = Arc::new(PresenceManager::new(event_bus.clone()));
    let mam_manager = Arc::new(MamManager::new(database.clone(), event_bus.clone()));

    spawn_component_task("roster", event_bus.clone(), {
        let manager = roster_manager.clone();
        async move { manager.run().await.map_err(|error| error.to_string()) }
    });

    spawn_component_task("messaging", event_bus.clone(), {
        let manager = message_manager.clone();
        async move { manager.run().await.map_err(|error| error.to_string()) }
    });

    spawn_component_task("muc", event_bus.clone(), {
        let manager = muc_manager.clone();
        async move { manager.run().await.map_err(|error| error.to_string()) }
    });

    spawn_component_task("presence", event_bus.clone(), {
        let manager = presence_manager.clone();
        async move { manager.run().await.map_err(|error| error.to_string()) }
    });

    spawn_component_task("mam", event_bus.clone(), {
        let manager = mam_manager.clone();
        async move { manager.run().await.map_err(|error| error.to_string()) }
    });

    let pipeline = Arc::new(build_stanza_pipeline(event_bus.clone()));
    let (wire_sender, wire_receiver) = stanza_channel(WIRE_CHANNEL_CAPACITY);
    let outbound_router = Arc::new(OutboundRouter::new(
        event_bus.clone(),
        pipeline.clone(),
        wire_sender,
    ));

    spawn_component_task("xmpp.outbound", event_bus.clone(), {
        let router = outbound_router.clone();
        async move { router.run().await.map_err(|error| error.to_string()) }
    });

    let connection = Arc::new(Mutex::new(ConnectionManager::with_event_bus(
        connection_config_from(&config),
        event_bus.clone(),
    )));

    spawn_wire_pump(connection.clone(), wire_receiver, event_bus.clone());
    spawn_inbound_pump(connection.clone(), pipeline, event_bus.clone());
    spawn_connection_control(connection.clone(), event_bus.clone());

    spawn_notifications(event_bus.clone(), config.clone());
    spawn_event_forwarder(event_bus.clone(), app_handle);

    publish_event(
        &event_bus,
        "system.startup.complete",
        EventSource::System(SYSTEM_COMPONENT.to_string()),
        EventPayload::StartupComplete,
    )?;

    spawn_initial_connection(connection.clone(), event_bus.clone());

    Ok(AppState {
        own_jid: Arc::new(Mutex::new(config.account.jid.clone())),
        ui_config,
        event_bus,
        connection_manager: connection,
        roster_manager,
        message_manager,
        muc_manager,
        mam_manager,
        presence_manager,
        plugin_registry,
        plugin_runtime,
    })
}

fn build_stanza_pipeline(event_bus: Arc<dyn EventBus>) -> StanzaPipeline {
    let mut pipeline = StanzaPipeline::new();
    pipeline.register(Box::new(RosterProcessor::new(event_bus.clone())));
    pipeline.register(Box::new(MessageProcessor::new(event_bus.clone())));
    pipeline.register(Box::new(PresenceProcessor::new(event_bus.clone())));
    pipeline.register(Box::new(MamProcessor::new(event_bus.clone())));
    pipeline.register(Box::new(MucProcessor::new(event_bus.clone())));
    pipeline.register(Box::new(ChatStateProcessor::new(event_bus.clone())));

    #[cfg(debug_assertions)]
    pipeline.register(Box::new(DebugProcessor::new(event_bus)));

    pipeline
}

fn spawn_component_task<F>(component: &'static str, event_bus: Arc<dyn EventBus>, task: F)
where
    F: Future<Output = Result<(), String>> + Send + 'static,
{
    tauri::async_runtime::spawn(async move {
        if let Err(reason) = task.await {
            error!(component, %reason, "component task terminated");
            emit_component_error(&event_bus, component, reason, true);
        }
    });
}

fn spawn_notifications(event_bus: Arc<dyn EventBus>, config: Config) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = NotificationManager::run(event_bus.clone(), &config).await {
            let reason = error.to_string();
            warn!(%reason, "notification manager terminated");
            emit_component_error(&event_bus, "notifications", reason, true);
        }
    });
}

fn spawn_wire_pump(
    connection: Arc<Mutex<ConnectionManager>>,
    mut wire_receiver: waddle_xmpp::StanzaReceiver,
    event_bus: Arc<dyn EventBus>,
) {
    tauri::async_runtime::spawn(async move {
        while let Some(stanza) = wire_receiver.recv().await {
            let send_result = {
                let mut manager = connection.lock().await;
                manager.send_stanza(&stanza).await
            };

            if let Err(error) = send_result {
                let reason = error.to_string();
                warn!(%reason, "failed to send stanza to XMPP transport");
                emit_component_error(&event_bus, "xmpp", reason.clone(), error.is_retryable());

                let recover_result = {
                    let mut manager = connection.lock().await;
                    manager.recover_after_network_interruption(reason).await
                };

                if let Err(recover_error) = recover_result {
                    emit_component_error(
                        &event_bus,
                        "xmpp",
                        recover_error.to_string(),
                        recover_error.is_retryable(),
                    );
                }
            }
        }

        debug!("wire pump stopped");
    });
}

fn spawn_inbound_pump(
    connection: Arc<Mutex<ConnectionManager>>,
    pipeline: Arc<StanzaPipeline>,
    event_bus: Arc<dyn EventBus>,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            let frame_result = {
                let mut manager = connection.lock().await;
                manager
                    .recv_frame_with_timeout(Duration::from_millis(50))
                    .await
            };

            let frame = match frame_result {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Err(error) => {
                    let reason = error.to_string();
                    warn!(%reason, "failed to receive stanza from XMPP transport");
                    emit_component_error(&event_bus, "xmpp", reason.clone(), error.is_retryable());

                    let recover_result = {
                        let mut manager = connection.lock().await;
                        manager.recover_after_network_interruption(reason).await
                    };

                    if let Err(recover_error) = recover_result {
                        emit_component_error(
                            &event_bus,
                            "xmpp",
                            recover_error.to_string(),
                            recover_error.is_retryable(),
                        );
                    }

                    continue;
                }
            };

            let stream_management_handled = {
                let mut manager = connection.lock().await;
                match manager.handle_stream_management_frame(&frame).await {
                    Ok(handled) => handled,
                    Err(error) => {
                        let reason = error.to_string();
                        warn!(%reason, "failed to handle stream-management frame");
                        emit_component_error(
                            &event_bus,
                            "xmpp",
                            reason.clone(),
                            error.is_retryable(),
                        );

                        let recover_result =
                            manager.recover_after_network_interruption(reason).await;
                        if let Err(recover_error) = recover_result {
                            emit_component_error(
                                &event_bus,
                                "xmpp",
                                recover_error.to_string(),
                                recover_error.is_retryable(),
                            );
                        }

                        continue;
                    }
                }
            };

            if stream_management_handled {
                continue;
            }

            let carbons_handled = {
                let mut manager = connection.lock().await;
                manager.handle_carbons_iq_response(&frame)
            };

            if carbons_handled {
                let mut manager = connection.lock().await;
                manager.mark_inbound_stanza_handled();
                continue;
            }

            let raw_stanza = String::from_utf8_lossy(&frame).into_owned();
            if let Err(error) = publish_event(
                &event_bus,
                "xmpp.raw.stanza.received",
                EventSource::Xmpp,
                EventPayload::RawStanzaReceived { stanza: raw_stanza },
            ) {
                warn!(%error, "failed to publish raw stanza event");
            }

            if let Err(error) = pipeline.process_inbound(&frame).await {
                warn!(error = %error, "failed to process inbound stanza");
                continue;
            }

            let mut manager = connection.lock().await;
            manager.mark_inbound_stanza_handled();
        }
    });
}

fn spawn_initial_connection(
    connection: Arc<Mutex<ConnectionManager>>,
    event_bus: Arc<dyn EventBus>,
) {
    tauri::async_runtime::spawn(async move {
        let connect_result = {
            let mut manager = connection.lock().await;
            manager.connect().await
        };

        if let Err(error) = connect_result {
            emit_component_error(&event_bus, "xmpp", error.to_string(), error.is_retryable());

            if !error.is_retryable() {
                let _ = publish_shutdown_requested(
                    &event_bus,
                    "non-recoverable authentication failure",
                );
            }
        }
    });
}

fn spawn_connection_control(
    connection: Arc<Mutex<ConnectionManager>>,
    event_bus: Arc<dyn EventBus>,
) {
    tauri::async_runtime::spawn(async move {
        let mut subscription = match event_bus.subscribe("system.**") {
            Ok(subscription) => subscription,
            Err(error) => {
                emit_component_error(&event_bus, "xmpp", error.to_string(), false);
                return;
            }
        };

        loop {
            match subscription.recv().await {
                Ok(event) => match event.payload {
                    EventPayload::ComingOnline => {
                        let connect_result = {
                            let mut manager = connection.lock().await;
                            manager.connect().await
                        };

                        if let Err(error) = connect_result {
                            emit_component_error(
                                &event_bus,
                                "xmpp",
                                error.to_string(),
                                error.is_retryable(),
                            );
                        }
                    }
                    EventPayload::GoingOffline => {
                        let disconnect_result = {
                            let mut manager = connection.lock().await;
                            manager.disconnect().await
                        };

                        if let Err(error) = disconnect_result {
                            emit_component_error(
                                &event_bus,
                                "xmpp",
                                error.to_string(),
                                error.is_retryable(),
                            );
                        }
                    }
                    EventPayload::ShutdownRequested { .. } => {
                        let mut unavailable_subscription =
                            match event_bus.subscribe("xmpp.presence.own_changed") {
                                Ok(subscription) => Some(subscription),
                                Err(error) => {
                                    emit_component_error(
                                        &event_bus,
                                        "presence",
                                        error.to_string(),
                                        true,
                                    );
                                    None
                                }
                            };

                        if let Err(error) = publish_event(
                            &event_bus,
                            "ui.presence.set",
                            EventSource::System(SYSTEM_COMPONENT.to_string()),
                            EventPayload::PresenceSetRequested {
                                show: PresenceShow::Unavailable,
                                status: None,
                            },
                        ) {
                            emit_component_error(&event_bus, "presence", error.to_string(), true);
                        }

                        let unavailable_seen = if let Some(mut unavailable_subscription) =
                            unavailable_subscription.take()
                        {
                            tokio::time::timeout(
                                Duration::from_secs(SHUTDOWN_CLEANUP_TIMEOUT_SECONDS),
                                async {
                                    loop {
                                        match unavailable_subscription.recv().await {
                                            Ok(event) => {
                                                if matches!(
                                                    event.payload,
                                                    EventPayload::OwnPresenceChanged {
                                                        show: PresenceShow::Unavailable,
                                                        ..
                                                    }
                                                ) {
                                                    return true;
                                                }
                                            }
                                            Err(waddle_core::error::EventBusError::Lagged(
                                                count,
                                            )) => {
                                                warn!(count, "shutdown presence wait lagged");
                                            }
                                            Err(
                                                waddle_core::error::EventBusError::ChannelClosed,
                                            ) => {
                                                return false;
                                            }
                                            Err(error) => {
                                                emit_component_error(
                                                    &event_bus,
                                                    "presence",
                                                    error.to_string(),
                                                    true,
                                                );
                                                return false;
                                            }
                                        }
                                    }
                                },
                            )
                            .await
                            .unwrap_or_default()
                        } else {
                            false
                        };

                        if !unavailable_seen {
                            warn!("timed out waiting for unavailable presence during shutdown");
                        }

                        let disconnect_result = {
                            let mut manager = connection.lock().await;
                            manager.disconnect().await
                        };

                        if let Err(error) = disconnect_result {
                            emit_component_error(
                                &event_bus,
                                "xmpp",
                                error.to_string(),
                                error.is_retryable(),
                            );
                        }

                        return;
                    }
                    _ => {}
                },
                Err(waddle_core::error::EventBusError::Lagged(count)) => {
                    warn!(count, "connection control lagged");
                }
                Err(waddle_core::error::EventBusError::ChannelClosed) => {
                    return;
                }
                Err(error) => {
                    emit_component_error(&event_bus, "xmpp", error.to_string(), false);
                    return;
                }
            }
        }
    });
}

fn frontend_event_name(channel: &str) -> String {
    channel.replace('.', ":")
}

fn spawn_event_forwarder(event_bus: Arc<dyn EventBus>, app_handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut subscription = match event_bus.subscribe("{xmpp,system,plugin}.**") {
            Ok(subscription) => subscription,
            Err(error) => {
                emit_component_error(&event_bus, "event-forwarder", error.to_string(), false);
                return;
            }
        };

        loop {
            match subscription.recv().await {
                Ok(event) => {
                    let channel = event.channel.as_str().to_string();
                    let frontend_channel = frontend_event_name(channel.as_str());
                    if let Err(error) = app_handle.emit(frontend_channel.as_str(), &event) {
                        warn!(channel, frontend_channel, %error, "failed to forward event to frontend");
                    }
                }
                Err(waddle_core::error::EventBusError::Lagged(count)) => {
                    warn!(count, "event forwarder lagged");
                }
                Err(waddle_core::error::EventBusError::ChannelClosed) => {
                    return;
                }
                Err(error) => {
                    emit_component_error(&event_bus, "event-forwarder", error.to_string(), false);
                    return;
                }
            }
        }
    });
}

async fn load_installed_plugins(
    plugin_registry: &PluginRegistry,
    plugin_runtime: &Arc<Mutex<PluginRuntime<NativeDatabase>>>,
    event_bus: &Arc<dyn EventBus>,
) {
    let installed = match plugin_registry.list_installed() {
        Ok(installed) => installed,
        Err(error) => {
            emit_component_error(event_bus, "plugins", error.to_string(), true);
            return;
        }
    };

    for plugin in installed {
        if let Err(error) =
            load_plugin_into_runtime(plugin_registry, plugin_runtime, &plugin.id).await
        {
            emit_component_error(event_bus, "plugins", error.to_string(), true);
        }
    }
}

async fn install_plugin(
    state: &AppState,
    reference: &str,
) -> Result<PluginInfoResponse, GuiBackendError> {
    let installed = state.plugin_registry.install(reference).await?;

    publish_event(
        &state.event_bus,
        "plugin.install.started",
        EventSource::System(SYSTEM_COMPONENT.to_string()),
        EventPayload::PluginInstallStarted {
            plugin_id: installed.id.clone(),
        },
    )?;

    let info = load_plugin_into_runtime(
        state.plugin_registry.as_ref(),
        &state.plugin_runtime,
        &installed.id,
    )
    .await?;

    publish_event(
        &state.event_bus,
        "plugin.install.completed",
        EventSource::System(SYSTEM_COMPONENT.to_string()),
        EventPayload::PluginInstallCompleted {
            plugin_id: installed.id,
        },
    )?;

    Ok(info)
}

async fn uninstall_plugin(
    state: &AppState,
    plugin_id: &str,
) -> Result<PluginInfoResponse, GuiBackendError> {
    let installed = state
        .plugin_registry
        .list_installed()?
        .into_iter()
        .find(|entry| entry.id == plugin_id);

    {
        let mut runtime = state.plugin_runtime.lock().await;
        if runtime.get_plugin(plugin_id).is_some() {
            runtime.unload_plugin(plugin_id).await?;
        }
    }

    state.plugin_registry.uninstall(plugin_id).await?;

    if let Some(installed) = installed {
        return Ok(PluginInfoResponse::from_installed(installed, "uninstalled"));
    }

    Ok(PluginInfoResponse {
        id: plugin_id.to_string(),
        name: plugin_id.to_string(),
        version: "unknown".to_string(),
        status: "uninstalled".to_string(),
        error_reason: None,
        error_count: 0,
        capabilities: Vec::new(),
    })
}

async fn update_plugin(
    state: &AppState,
    plugin_id: &str,
) -> Result<PluginInfoResponse, GuiBackendError> {
    if state.plugin_registry.update(plugin_id).await?.is_some() {
        return load_plugin_into_runtime(
            state.plugin_registry.as_ref(),
            &state.plugin_runtime,
            plugin_id,
        )
        .await;
    }

    get_plugin(state, plugin_id).await
}

async fn get_plugin(
    state: &AppState,
    plugin_id: &str,
) -> Result<PluginInfoResponse, GuiBackendError> {
    {
        let runtime = state.plugin_runtime.lock().await;
        if let Some(plugin) = runtime.get_plugin(plugin_id) {
            return Ok(PluginInfoResponse::from_runtime(plugin.clone()));
        }
    }

    let installed = state
        .plugin_registry
        .list_installed()?
        .into_iter()
        .find(|entry| entry.id == plugin_id)
        .ok_or_else(|| GuiBackendError::CommandFailed {
            command: "manage_plugins.get".to_string(),
            reason: format!("plugin '{plugin_id}' not found"),
        })?;

    Ok(PluginInfoResponse::from_installed(installed, "installed"))
}

async fn load_plugin_into_runtime(
    plugin_registry: &PluginRegistry,
    plugin_runtime: &Arc<Mutex<PluginRuntime<NativeDatabase>>>,
    plugin_id: &str,
) -> Result<PluginInfoResponse, GuiBackendError> {
    let files = plugin_registry.get_plugin_files(plugin_id)?;
    let wasm_bytes = tokio::fs::read(&files.wasm_path).await?;

    let mut runtime = plugin_runtime.lock().await;

    if runtime.get_plugin(plugin_id).is_some() {
        runtime.unload_plugin(plugin_id).await?;
    }

    runtime.load_plugin(files.manifest, &wasm_bytes).await?;

    let plugin =
        runtime
            .get_plugin(plugin_id)
            .cloned()
            .ok_or_else(|| GuiBackendError::CommandFailed {
                command: "manage_plugins.load".to_string(),
                reason: format!(
                    "plugin '{plugin_id}' was loaded but no runtime metadata is available"
                ),
            })?;

    Ok(PluginInfoResponse::from_runtime(plugin))
}

fn capability_label(capability: &PluginCapability) -> String {
    match capability {
        PluginCapability::EventHandler => "event-handler".to_string(),
        PluginCapability::StanzaProcessor { priority } => {
            format!("stanza-processor:{priority}")
        }
        PluginCapability::TuiRenderer => "tui-renderer".to_string(),
        PluginCapability::GuiMetadata => "gui-metadata".to_string(),
        PluginCapability::GuiRenderer => "gui-renderer".to_string(),
        PluginCapability::MessageTransformer => "message-transformer".to_string(),
    }
}

fn parse_presence_show(value: &str) -> Option<PresenceShow> {
    match value.trim().to_ascii_lowercase().as_str() {
        "available" => Some(PresenceShow::Available),
        "chat" => Some(PresenceShow::Chat),
        "away" => Some(PresenceShow::Away),
        "xa" => Some(PresenceShow::Xa),
        "dnd" => Some(PresenceShow::Dnd),
        "unavailable" => Some(PresenceShow::Unavailable),
        _ => None,
    }
}

fn connection_config_from(config: &Config) -> ConnectionConfig {
    ConnectionConfig {
        jid: config.account.jid.clone(),
        password: config.account.password.clone(),
        server: config.account.server.clone(),
        port: config.account.port,
        timeout_seconds: CONNECTION_TIMEOUT_SECONDS,
        max_reconnect_attempts: CONNECTION_MAX_RECONNECT_ATTEMPTS,
    }
}

fn resolve_storage_path(config: &Config) -> PathBuf {
    config
        .storage
        .path
        .as_deref()
        .map(expand_home_path)
        .unwrap_or_else(default_storage_path)
}

fn default_storage_path() -> PathBuf {
    if let Some(project_dirs) = ProjectDirs::from("com", "waddle", "waddle") {
        project_dirs.data_dir().join("waddle.db")
    } else {
        PathBuf::from("waddle.db")
    }
}

fn resolve_plugin_data_dir(config: &Config) -> PathBuf {
    if let Some(configured_path) = config.plugins.directory.as_deref() {
        let plugin_path = expand_home_path(configured_path);
        if plugin_path
            .file_name()
            .is_some_and(|file_name| file_name == "plugins")
        {
            if let Some(parent) = plugin_path.parent() {
                return parent.to_path_buf();
            }
        }

        return plugin_path;
    }

    if let Some(project_dirs) = ProjectDirs::from("com", "waddle", "waddle") {
        project_dirs.data_dir().to_path_buf()
    } else {
        PathBuf::from(".")
    }
}

fn expand_home_path(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix("~/")
        && let Some(base_dirs) = BaseDirs::new()
    {
        return base_dirs.home_dir().join(stripped);
    }

    PathBuf::from(path)
}

fn publish_event(
    event_bus: &Arc<dyn EventBus>,
    channel_name: &str,
    source: EventSource,
    payload: EventPayload,
) -> Result<(), GuiBackendError> {
    let event = Event::new(Channel::new(channel_name)?, source, payload);
    event_bus.publish(event)?;
    Ok(())
}

fn publish_shutdown_requested(
    event_bus: &Arc<dyn EventBus>,
    reason: &str,
) -> Result<(), GuiBackendError> {
    publish_event(
        event_bus,
        "system.shutdown.requested",
        EventSource::System(SYSTEM_COMPONENT.to_string()),
        EventPayload::ShutdownRequested {
            reason: reason.to_string(),
        },
    )
}

fn emit_component_error(
    event_bus: &Arc<dyn EventBus>,
    component: &str,
    message: String,
    recoverable: bool,
) {
    let result = publish_event(
        event_bus,
        "system.error.occurred",
        EventSource::System(SYSTEM_COMPONENT.to_string()),
        EventPayload::ErrorOccurred {
            component: component.to_string(),
            message,
            recoverable,
        },
    );

    if let Err(error) = result {
        error!(%error, "failed to publish component error");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use waddle_core::event::MessageType;

    fn make_test_message(id: &str) -> ChatMessage {
        ChatMessage {
            id: id.to_string(),
            from: "alice@example.com".to_string(),
            to: "bob@example.com".to_string(),
            body: "hello".to_string(),
            timestamp: "2024-01-01T00:00:00Z"
                .parse()
                .expect("static test timestamp should parse"),
            message_type: MessageType::Chat,
            thread: None,
            embeds: vec![],
        }
    }

    #[test]
    fn parses_presence_show_values() {
        assert!(matches!(
            parse_presence_show("available"),
            Some(PresenceShow::Available)
        ));
        assert!(matches!(
            parse_presence_show("chat"),
            Some(PresenceShow::Chat)
        ));
        assert!(matches!(
            parse_presence_show("away"),
            Some(PresenceShow::Away)
        ));
        assert!(matches!(parse_presence_show("xa"), Some(PresenceShow::Xa)));
        assert!(matches!(
            parse_presence_show("dnd"),
            Some(PresenceShow::Dnd)
        ));
        assert!(matches!(
            parse_presence_show("unavailable"),
            Some(PresenceShow::Unavailable)
        ));
        assert!(parse_presence_show("invalid").is_none());
    }

    #[test]
    fn expands_home_paths() {
        let expanded = expand_home_path("~/waddle/test");
        assert!(expanded.ends_with("waddle/test"));
    }

    #[test]
    fn defaults_storage_path_to_waddle_db() {
        let path = default_storage_path();
        assert!(path.ends_with("waddle.db"));
    }

    #[tokio::test]
    async fn history_fallback_skips_remote_when_local_present() {
        let remote_called = Arc::new(AtomicBool::new(false));
        let messages = vec![make_test_message("local-1")];

        let result = with_remote_history_fallback(
            || {
                let messages = messages.clone();
                async move { Ok(messages) }
            },
            || {
                let remote_called = remote_called.clone();
                async move {
                    remote_called.store(true, Ordering::Relaxed);
                    Ok(())
                }
            },
        )
        .await
        .expect("history fallback should succeed");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "local-1");
        assert!(!remote_called.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn history_fallback_refetches_local_after_remote_fetch() {
        let local_calls = Arc::new(AtomicUsize::new(0));
        let remote_calls = Arc::new(AtomicUsize::new(0));

        let result = with_remote_history_fallback(
            || {
                let local_calls = local_calls.clone();
                async move {
                    let call_idx = local_calls.fetch_add(1, Ordering::Relaxed);
                    if call_idx == 0 {
                        Ok(Vec::new())
                    } else {
                        Ok(vec![make_test_message("from-remote-1")])
                    }
                }
            },
            || {
                let remote_calls = remote_calls.clone();
                async move {
                    remote_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }
            },
        )
        .await
        .expect("history fallback should succeed");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "from-remote-1");
        assert_eq!(local_calls.load(Ordering::Relaxed), 2);
        assert_eq!(remote_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn history_fallback_returns_local_when_remote_fetch_fails() {
        let local_calls = Arc::new(AtomicUsize::new(0));
        let remote_calls = Arc::new(AtomicUsize::new(0));

        let result = with_remote_history_fallback(
            || {
                let local_calls = local_calls.clone();
                async move {
                    let call_idx = local_calls.fetch_add(1, Ordering::Relaxed);
                    if call_idx == 0 {
                        Ok(Vec::new())
                    } else {
                        Ok(vec![make_test_message("local-after-failure")])
                    }
                }
            },
            || {
                let remote_calls = remote_calls.clone();
                async move {
                    remote_calls.fetch_add(1, Ordering::Relaxed);
                    Err("MAM unavailable".to_string())
                }
            },
        )
        .await
        .expect("history fallback should return local history after remote failure");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "local-after-failure");
        assert_eq!(local_calls.load(Ordering::Relaxed), 2);
        assert_eq!(remote_calls.load(Ordering::Relaxed), 1);
    }
}
