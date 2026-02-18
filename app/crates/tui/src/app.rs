use std::sync::Arc;

use crossterm::event::{self, Event as CrosstermEvent, EventStream};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::select;

use waddle_core::config::Config;
use waddle_core::event::{
    Channel, ChatMessage, Event, EventBus, EventPayload, EventSource, PresenceShow, UiTarget,
};
use waddle_core::i18n::I18n;
use waddle_core::theme::ThemeManager;

use crate::error::TuiError;
use crate::input::{self, Action};
use crate::state::{AppState, ConnectionStatus, MucRoom, RosterEntry};
use crate::ui;

pub struct TuiApp;

impl TuiApp {
    pub async fn run(event_bus: Arc<dyn EventBus>, config: &Config) -> Result<(), TuiError> {
        let mut terminal =
            ratatui::try_init().map_err(|e| TuiError::TerminalInit(e.to_string()))?;
        let state = initial_state(config)?;
        let result = run_loop(&mut terminal, event_bus, state).await;
        ratatui::restore();
        result
    }
}

fn initial_state(config: &Config) -> Result<AppState, TuiError> {
    let i18n = I18n::new(config.ui.locale.as_deref(), &["en-US"]);
    let theme = ThemeManager::load(&config.theme).map_err(TuiError::Theme)?;

    let mut theme_manager = ThemeManager::new();
    if ThemeManager::builtin(&theme.name).is_none() {
        theme_manager.register_custom(theme.clone());
    }

    Ok(AppState::new(i18n, theme_manager, theme))
}

async fn run_loop(
    terminal: &mut DefaultTerminal,
    event_bus: Arc<dyn EventBus>,
    mut state: AppState,
) -> Result<(), TuiError> {
    let mut event_sub = event_bus.subscribe("**").map_err(TuiError::EventBus)?;
    let mut reader = EventStream::new();

    loop {
        terminal
            .draw(|frame| ui::draw(frame, &state))
            .map_err(|e| TuiError::Render(e.to_string()))?;

        select! {
            crossterm_event = reader.next() => {
                if let Some(Ok(CrosstermEvent::Key(key))) = crossterm_event {
                    if key.kind != event::KeyEventKind::Press {
                        continue;
                    }
                    let action = input::handle_key(&mut state, key);
                    handle_action(&event_bus, &mut state, action)?;
                    if state.should_quit {
                        break;
                    }
                }
            }
            bus_event = event_sub.recv() => {
                match bus_event {
                    Ok(event) => handle_bus_event(&mut state, event),
                    Err(waddle_core::error::EventBusError::Lagged(n)) => {
                        tracing::warn!("TUI event subscription lagged, missed {n} events");
                    }
                    Err(waddle_core::error::EventBusError::ChannelClosed) => {
                        break;
                    }
                    Err(e) => {
                        tracing::error!("Event bus error: {e}");
                    }
                }
            }
        }
    }

    Ok(())
}

fn handle_action(
    event_bus: &Arc<dyn EventBus>,
    state: &mut AppState,
    action: Action,
) -> Result<(), TuiError> {
    match action {
        Action::None => {}
        Action::Quit => {}
        Action::OpenConversation(jid) => {
            publish(
                event_bus,
                "ui.conversation.opened",
                EventPayload::ConversationOpened { jid },
            )?;
        }
        Action::SendMessage { to, body } => {
            let is_room = state.rooms.iter().any(|r| r.jid == to);
            if is_room {
                publish(
                    event_bus,
                    "ui.message.send",
                    EventPayload::MucSendRequested { room: to, body },
                )?;
            } else {
                publish(
                    event_bus,
                    "ui.message.send",
                    EventPayload::MessageSendRequested {
                        to,
                        body,
                        message_type: waddle_core::event::MessageType::Chat,
                    },
                )?;
            }
        }
        Action::ExecuteCommand(cmd) => {
            handle_command(event_bus, state, &cmd)?;
        }
    }
    Ok(())
}

fn handle_command(
    event_bus: &Arc<dyn EventBus>,
    state: &mut AppState,
    cmd: &str,
) -> Result<(), TuiError> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let (raw_head, tail) = split_head_tail(trimmed);
    let head = raw_head.to_ascii_lowercase();

    if let Some(show) = parse_presence_show(&head) {
        publish(
            event_bus,
            "ui.presence.set",
            EventPayload::PresenceSetRequested {
                show: show.clone(),
                status: non_empty_string(tail),
            },
        )?;

        let label = state.i18n.t(presence_message_id(&show), None);
        let prefix = state.i18n.t("command-presence-updated", None);
        state.command_feedback = Some(format!("{prefix} {label}"));
        return Ok(());
    }

    match head.as_str() {
        "quit" | "q" => {
            state.should_quit = true;
        }
        "help" | "h" => {
            state.command_feedback = Some(command_help_text(state));
        }
        "status" | "presence" => {
            let (show_token, status_tail) = split_head_tail(tail);
            if show_token.is_empty() {
                state.command_feedback = Some(state.i18n.t("command-presence-usage", None));
                return Ok(());
            }

            let Some(show) = parse_presence_show(&show_token.to_ascii_lowercase()) else {
                state.command_feedback = Some(state.i18n.t("command-presence-usage", None));
                return Ok(());
            };

            publish(
                event_bus,
                "ui.presence.set",
                EventPayload::PresenceSetRequested {
                    show: show.clone(),
                    status: non_empty_string(status_tail),
                },
            )?;

            let label = state.i18n.t(presence_message_id(&show), None);
            let prefix = state.i18n.t("command-presence-updated", None);
            state.command_feedback = Some(format!("{prefix} {label}"));
        }
        "join" => {
            let (room, nick_tail) = split_head_tail(tail);
            if room.is_empty() {
                state.command_feedback = Some(state.i18n.t("command-join-usage", None));
                return Ok(());
            }

            let nick = if nick_tail.is_empty() {
                default_nick(state).to_string()
            } else {
                nick_tail.to_string()
            };

            publish(
                event_bus,
                "ui.muc.join",
                EventPayload::MucJoinRequested {
                    room: room.to_string(),
                    nick,
                },
            )?;

            let prefix = state.i18n.t("command-joining-room", None);
            state.command_feedback = Some(format!("{prefix} {room}"));
        }
        "leave" => {
            let room = if !tail.is_empty() {
                Some(tail.to_string())
            } else {
                state
                    .active_conversation
                    .as_ref()
                    .filter(|active| state.rooms.iter().any(|r| r.jid == **active))
                    .cloned()
            };

            let Some(room) = room else {
                state.command_feedback = Some(state.i18n.t("command-leave-usage", None));
                return Ok(());
            };

            publish(
                event_bus,
                "ui.muc.leave",
                EventPayload::MucLeaveRequested { room: room.clone() },
            )?;

            let prefix = state.i18n.t("command-leaving-room", None);
            state.command_feedback = Some(format!("{prefix} {room}"));
        }
        "theme" => {
            let theme_id = tail.trim();
            if theme_id.is_empty() {
                state.command_feedback = Some(state.i18n.t("command-theme-usage", None));
                return Ok(());
            }

            let Some(theme) = state.theme_manager.get(theme_id) else {
                let prefix = state.i18n.t("command-theme-not-found", None);
                state.command_feedback = Some(format!("{prefix} {theme_id}"));
                return Ok(());
            };

            publish(
                event_bus,
                "ui.theme.changed",
                EventPayload::ThemeChanged {
                    theme_id: theme_id.to_string(),
                },
            )?;

            state.theme = theme;
            let prefix = state.i18n.t("command-theme-switched", None);
            state.command_feedback = Some(format!("{prefix} {theme_id}"));
        }
        _ => {
            let prefix = state.i18n.t("command-unknown", None);
            state.command_feedback = Some(format!("{prefix} {raw_head}"));
        }
    }

    Ok(())
}

fn parse_presence_show(value: &str) -> Option<PresenceShow> {
    match value {
        "available" => Some(PresenceShow::Available),
        "away" => Some(PresenceShow::Away),
        "dnd" => Some(PresenceShow::Dnd),
        "xa" => Some(PresenceShow::Xa),
        "chat" => Some(PresenceShow::Chat),
        _ => None,
    }
}

fn presence_message_id(show: &PresenceShow) -> &'static str {
    match show {
        PresenceShow::Available | PresenceShow::Chat => "status-available",
        PresenceShow::Away => "status-away",
        PresenceShow::Xa => "status-xa",
        PresenceShow::Dnd => "status-dnd",
        PresenceShow::Unavailable => "status-unavailable",
    }
}

fn split_head_tail(input: &str) -> (&str, &str) {
    let trimmed = input.trim();
    if let Some(index) = trimmed.find(char::is_whitespace) {
        (&trimmed[..index], trimmed[index..].trim())
    } else {
        (trimmed, "")
    }
}

fn non_empty_string(input: &str) -> Option<String> {
    if input.is_empty() {
        None
    } else {
        Some(input.to_string())
    }
}

fn default_nick(state: &AppState) -> &str {
    state
        .connected_jid
        .as_deref()
        .and_then(|jid| jid.split('@').next())
        .filter(|nick| !nick.is_empty())
        .unwrap_or("waddle-user")
}

fn command_help_text(state: &AppState) -> String {
    let status_usage = state.i18n.t("command-presence-usage", None);

    format!(
        ":help ({}) | :quit ({}) | :status ({status_usage}) | :join ({}) | :leave ({}) | :theme ({})",
        state.i18n.t("cmd-help", None),
        state.i18n.t("cmd-quit", None),
        state.i18n.t("cmd-join", None),
        state.i18n.t("cmd-leave", None),
        state.i18n.t("cmd-theme", None),
    )
}

fn handle_bus_event(state: &mut AppState, event: Event) {
    match event.payload {
        EventPayload::RosterReceived { items } => {
            state.roster = items
                .into_iter()
                .map(|item| RosterEntry {
                    item,
                    presence: PresenceShow::Unavailable,
                    unread: 0,
                })
                .collect();
        }
        EventPayload::RosterUpdated { item } => {
            if let Some(entry) = state.roster.iter_mut().find(|e| e.item.jid == item.jid) {
                entry.item = item;
            } else {
                state.roster.push(RosterEntry {
                    item,
                    presence: PresenceShow::Unavailable,
                    unread: 0,
                });
            }
        }
        EventPayload::RosterRemoved { jid } => {
            state.roster.retain(|e| e.item.jid != jid);
            if state.sidebar_index >= state.sidebar_items_count() && state.sidebar_index > 0 {
                state.sidebar_index -= 1;
            }
        }
        EventPayload::PresenceChanged { jid, show, .. } => {
            let bare_jid = jid.split('/').next().unwrap_or(&jid);
            if let Some(entry) = state.roster.iter_mut().find(|e| e.item.jid == bare_jid) {
                entry.presence = show;
            }
        }
        EventPayload::MessageReceived { message } => {
            let from_bare = message
                .from
                .split('/')
                .next()
                .unwrap_or(&message.from)
                .to_string();
            add_message(state, &from_bare, message);
        }
        EventPayload::MessageSent { message } => {
            let to_bare = message
                .to
                .split('/')
                .next()
                .unwrap_or(&message.to)
                .to_string();
            add_message(state, &to_bare, message);
        }
        EventPayload::MessageDelivered { id, .. } => {
            state.delivered_message_ids.insert(id);
        }
        EventPayload::ChatStateReceived { from, state: cs } => {
            let bare = from.split('/').next().unwrap_or(&from);
            if let Some(conv) = state.conversations.get_mut(bare) {
                conv.remote_chat_state = Some(cs);
            }
        }
        EventPayload::MucMessageReceived { room, message } => {
            add_message(state, &room, message);
        }
        EventPayload::MucJoined { room, .. } => {
            if !state.rooms.iter().any(|r| r.jid == room) {
                let name = room.split('@').next().unwrap_or(&room).to_string();
                state.rooms.push(MucRoom {
                    jid: room,
                    name,
                    unread: 0,
                });
            }
        }
        EventPayload::MucLeft { room } => {
            state.rooms.retain(|r| r.jid != room);
            if state.active_conversation.as_deref() == Some(&room) {
                state.active_conversation = None;
            }
            if state.sidebar_index >= state.sidebar_items_count() && state.sidebar_index > 0 {
                state.sidebar_index -= 1;
            }
        }
        EventPayload::MucSubjectChanged { room, subject } => {
            if let Some(r) = state.rooms.iter_mut().find(|r| r.jid == room) {
                r.name = if subject.is_empty() {
                    room.split('@').next().unwrap_or(&room).to_string()
                } else {
                    subject
                };
            }
        }
        EventPayload::ConnectionEstablished { jid } => {
            state.connected_jid = Some(jid.clone());
            state.connection_status = ConnectionStatus::Connected { jid };
        }
        EventPayload::ConnectionLost { .. } => {
            state.connection_status = ConnectionStatus::Disconnected;
        }
        EventPayload::ConnectionReconnecting { .. } => {
            state.connection_status = ConnectionStatus::Connecting;
        }
        EventPayload::SyncStarted => {
            state.connection_status = ConnectionStatus::Syncing;
        }
        EventPayload::SyncCompleted { .. } => {
            if let ConnectionStatus::Syncing = state.connection_status {
                state.connection_status = match state.connected_jid.clone() {
                    Some(jid) => ConnectionStatus::Connected { jid },
                    None => ConnectionStatus::Disconnected,
                };
            }
        }
        EventPayload::ThemeChanged { theme_id } => {
            if let Some(theme) = state.theme_manager.get(&theme_id) {
                state.theme = theme;
            }
        }
        _ => {}
    }
}

fn add_message(state: &mut AppState, conversation_jid: &str, message: ChatMessage) {
    let conv = state.ensure_conversation(conversation_jid);
    conv.remote_chat_state = None;
    conv.messages.push(message);

    if state.active_conversation.as_deref() != Some(conversation_jid) {
        if let Some(entry) = state
            .roster
            .iter_mut()
            .find(|e| e.item.jid == conversation_jid)
        {
            entry.unread += 1;
        }
        if let Some(room) = state.rooms.iter_mut().find(|r| r.jid == conversation_jid) {
            room.unread += 1;
        }
    }
}

fn publish(
    event_bus: &Arc<dyn EventBus>,
    channel: &str,
    payload: EventPayload,
) -> Result<(), TuiError> {
    let channel = Channel::new(channel).map_err(TuiError::EventBus)?;
    let event = Event::new(channel, EventSource::Ui(UiTarget::Tui), payload);
    event_bus.publish(event).map_err(TuiError::EventBus)?;
    Ok(())
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;

    use std::time::Duration;

    use tokio::time::timeout;
    use waddle_core::config::ThemeConfig;
    use waddle_core::event::{BroadcastEventBus, EventBus};

    fn test_state() -> AppState {
        let i18n = I18n::new(Some("en-US"), &["en-US"]);
        let theme = ThemeManager::load(&ThemeConfig::default()).unwrap();
        let mut theme_manager = ThemeManager::new();
        if ThemeManager::builtin(&theme.name).is_none() {
            theme_manager.register_custom(theme.clone());
        }

        AppState::new(i18n, theme_manager, theme)
    }

    fn test_event_bus() -> Arc<dyn EventBus> {
        Arc::new(BroadcastEventBus::default())
    }

    #[tokio::test]
    async fn command_status_publishes_presence_set() {
        let event_bus = test_event_bus();
        let mut sub = event_bus.subscribe("ui.presence.set").unwrap();
        let mut state = test_state();

        handle_command(&event_bus, &mut state, "status away in a meeting").unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(
            event.payload,
            EventPayload::PresenceSetRequested {
                show: PresenceShow::Away,
                status: Some(status),
            } if status == "in a meeting"
        ));
    }

    #[tokio::test]
    async fn command_theme_publishes_theme_changed_and_updates_state() {
        let event_bus = test_event_bus();
        let mut sub = event_bus.subscribe("ui.theme.changed").unwrap();
        let mut state = test_state();

        handle_command(&event_bus, &mut state, "theme dark").unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(
            event.payload,
            EventPayload::ThemeChanged { theme_id } if theme_id == "dark"
        ));
        assert_eq!(state.theme.name, "dark");
    }

    #[tokio::test]
    async fn command_join_publishes_muc_join_request() {
        let event_bus = test_event_bus();
        let mut sub = event_bus.subscribe("ui.muc.join").unwrap();
        let mut state = test_state();

        handle_command(
            &event_bus,
            &mut state,
            "join general@conference.example.com Alice",
        )
        .unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(
            event.payload,
            EventPayload::MucJoinRequested { room, nick }
                if room == "general@conference.example.com" && nick == "Alice"
        ));
    }

    #[tokio::test]
    async fn command_leave_uses_active_room_when_no_arg() {
        let event_bus = test_event_bus();
        let mut sub = event_bus.subscribe("ui.muc.leave").unwrap();
        let mut state = test_state();

        let room = "general@conference.example.com".to_string();
        state.rooms.push(MucRoom {
            jid: room.clone(),
            name: "general".to_string(),
            unread: 0,
        });
        state.active_conversation = Some(room.clone());

        handle_command(&event_bus, &mut state, "leave").unwrap();

        let event = timeout(Duration::from_millis(100), sub.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(
            event.payload,
            EventPayload::MucLeaveRequested { room: payload_room } if payload_room == room
        ));
    }
}
