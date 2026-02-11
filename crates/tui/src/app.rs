use std::sync::Arc;

use crossterm::event::{self, Event as CrosstermEvent, EventStream};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::select;

use waddle_core::config::Config;
use waddle_core::event::{
    Channel, ChatMessage, Event, EventBus, EventPayload, EventSource, PresenceShow, UiTarget,
};

use crate::error::TuiError;
use crate::input::{self, Action};
use crate::state::{AppState, ConnectionStatus, MucRoom, RosterEntry};
use crate::ui;

pub struct TuiApp;

impl TuiApp {
    pub async fn run(event_bus: Arc<dyn EventBus>, _config: &Config) -> Result<(), TuiError> {
        let mut terminal =
            ratatui::try_init().map_err(|e| TuiError::TerminalInit(e.to_string()))?;
        let result = run_loop(&mut terminal, event_bus).await;
        ratatui::restore();
        result
    }
}

async fn run_loop(
    terminal: &mut DefaultTerminal,
    event_bus: Arc<dyn EventBus>,
) -> Result<(), TuiError> {
    let mut state = AppState::new();

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
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    match parts[0] {
        "quit" | "q" => {
            state.should_quit = true;
        }
        "available" | "away" | "dnd" | "xa" | "chat" => {
            let show = match parts[0] {
                "available" => PresenceShow::Available,
                "away" => PresenceShow::Away,
                "dnd" => PresenceShow::Dnd,
                "xa" => PresenceShow::Xa,
                "chat" => PresenceShow::Chat,
                _ => unreachable!(),
            };
            let status = parts.get(1).map(|s| s.to_string());
            publish(
                event_bus,
                "ui.presence.set",
                EventPayload::PresenceSetRequested { show, status },
            )?;
        }
        "join" => {
            if let Some(room_arg) = parts.get(1) {
                let room_parts: Vec<&str> = room_arg.splitn(2, ' ').collect();
                let room = room_parts[0].to_string();
                let nick = room_parts.get(1).unwrap_or(&"waddle-user").to_string();
                publish(
                    event_bus,
                    "ui.muc.join",
                    EventPayload::MucJoinRequested { room, nick },
                )?;
            }
        }
        "leave" => {
            if let Some(room) = parts.get(1) {
                publish(
                    event_bus,
                    "ui.muc.leave",
                    EventPayload::MucLeaveRequested {
                        room: room.to_string(),
                    },
                )?;
            } else if let Some(active) = &state.active_conversation {
                if state.rooms.iter().any(|r| r.jid == *active) {
                    publish(
                        event_bus,
                        "ui.muc.leave",
                        EventPayload::MucLeaveRequested {
                            room: active.clone(),
                        },
                    )?;
                }
            }
        }
        "theme" => {
            if let Some(theme_id) = parts.get(1) {
                publish(
                    event_bus,
                    "ui.theme.changed",
                    EventPayload::ThemeChanged {
                        theme_id: theme_id.to_string(),
                    },
                )?;
            }
        }
        _ => {}
    }
    Ok(())
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
