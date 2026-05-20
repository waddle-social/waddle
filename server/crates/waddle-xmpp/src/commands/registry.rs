//! Command Registry for XEP-0050 Ad-Hoc Commands
//!
//! Manages command registration, session state, and dispatching for ad-hoc commands.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jid::Jid;
use tokio::sync::RwLock;
use tracing::debug;
use uuid::Uuid;
use xmpp_parsers::iq::Iq;

use crate::xep::xep0004::DataForm;
use crate::xep::xep0050::{Action, Command, Note};
use crate::XmppError;

/// Time-to-live for command sessions (5 minutes)
const SESSION_TTL: Duration = Duration::from_secs(300);

/// Result of executing a command handler.
pub enum CommandResult {
    /// Command is still executing, return a form for the next step.
    Executing {
        form: DataForm,
        session_id: String,
        notes: Vec<Note>,
    },
    /// Command completed successfully.
    Completed {
        form: Option<DataForm>,
        notes: Vec<Note>,
    },
    /// Command was canceled.
    Canceled { notes: Vec<Note> },
    /// Command failed with an error.
    Error(XmppError),
}

/// Context provided to command handlers.
pub struct CommandContext {
    /// The requester's JID
    pub from: Jid,
    /// The authenticated internal principal for this connection.
    pub authenticated_user_id: Option<String>,
    /// The original IQ request
    pub iq: Iq,
    /// The parsed command
    pub command: Command,
}

/// A command handler function.
///
/// Handlers receive the command context and must return a `CommandResult`.
/// The handler is responsible for:
/// - Validating permissions
/// - Processing command logic
/// - Returning appropriate forms or completion status
pub type CommandHandler = Arc<
    dyn Fn(
            CommandContext,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = CommandResult> + Send + 'static>>
        + Send
        + Sync,
>;

/// Command metadata for discovery.
pub struct CommandMetadata {
    /// Command node identifier
    pub node: String,
    /// Human-readable command name
    pub name: String,
    /// Handler function
    pub handler: CommandHandler,
}

/// Active command session tracking state.
#[derive(Clone)]
pub struct CommandSession {
    /// Session ID
    pub id: String,
    /// When this session was created
    pub created_at: Instant,
    /// When this session was last accessed
    pub last_accessed: Instant,
    /// Command node identifier
    pub node: String,
    /// Requester JID
    pub from: Jid,
    /// Custom session state (opaque to the registry)
    pub state: HashMap<String, String>,
}

impl CommandSession {
    fn new(id: String, node: String, from: Jid) -> Self {
        let now = Instant::now();
        Self {
            id,
            created_at: now,
            last_accessed: now,
            node,
            from,
            state: HashMap::new(),
        }
    }

    fn touch(&mut self) {
        self.last_accessed = Instant::now();
    }

    fn is_expired(&self) -> bool {
        self.last_accessed.elapsed() > SESSION_TTL
    }
}

/// Registry for ad-hoc commands.
pub struct CommandRegistry {
    /// Registered command metadata by node
    commands: RwLock<HashMap<String, CommandMetadata>>,
    /// Active command sessions
    sessions: RwLock<HashMap<String, CommandSession>>,
}

impl CommandRegistry {
    /// Create a new command registry.
    pub fn new() -> Self {
        Self {
            commands: RwLock::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Register a command handler.
    ///
    /// # Arguments
    /// - `node`: The command node identifier (e.g., "waddle:create-channel")
    /// - `name`: The human-readable command name (e.g., "Create Channel")
    /// - `handler`: The handler function
    pub async fn register<F, Fut>(
        &self,
        node: impl Into<String>,
        name: impl Into<String>,
        handler: F,
    ) where
        F: Fn(CommandContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = CommandResult> + Send + 'static,
    {
        let node = node.into();
        let name = name.into();
        let wrapped: CommandHandler = Arc::new(move |ctx| Box::pin(handler(ctx)));
        let metadata = CommandMetadata {
            node: node.clone(),
            name: name.clone(),
            handler: wrapped,
        };
        self.commands.write().await.insert(node.clone(), metadata);
        debug!(node = %node, name = %name, "Registered ad-hoc command");
    }

    /// List all registered commands with their names.
    /// Returns (node, name) tuples.
    pub async fn list_commands(&self) -> Vec<(String, String)> {
        self.commands
            .read()
            .await
            .values()
            .map(|cmd| (cmd.node.clone(), cmd.name.clone()))
            .collect()
    }

    /// Check if a command is registered.
    pub async fn has_command(&self, node: &str) -> bool {
        self.commands.read().await.contains_key(node)
    }

    /// Dispatch a command request.
    ///
    /// This function:
    /// 1. Validates the command exists
    /// 2. Creates or retrieves a session
    /// 3. Calls the handler
    /// 4. Cleans up expired sessions
    pub async fn dispatch(&self, mut ctx: CommandContext) -> CommandResult {
        let node = &ctx.command.node;
        let from = &ctx.from;

        // Clean up expired sessions periodically
        self.cleanup_expired_sessions().await;

        // Get the handler
        let handler = {
            let commands = self.commands.read().await;
            match commands.get(node) {
                Some(cmd) => Arc::clone(&cmd.handler),
                None => {
                    return CommandResult::Error(XmppError::service_unavailable(Some(format!(
                        "Command '{}' not found",
                        node
                    ))));
                }
            }
        };

        // Handle session creation/retrieval
        let action = ctx.command.action.unwrap_or_else(|| {
            if ctx.command.session_id.is_some() {
                Action::Next
            } else {
                Action::Execute
            }
        });
        let mut created_session_id = None;
        match action {
            Action::Execute => {
                if ctx.command.session_id.is_some() {
                    return CommandResult::Error(XmppError::bad_request(Some(
                        "execute must not include an existing sessionid".to_string(),
                    )));
                }
                // Create a new session
                let session_id = Uuid::new_v4().to_string();
                let session = CommandSession::new(session_id.clone(), node.clone(), from.clone());
                self.sessions
                    .write()
                    .await
                    .insert(session_id.clone(), session);
                debug!(
                    node = %node,
                    session_id = %session_id,
                    from = %from,
                    "Created command session"
                );
                ctx.command.session_id = Some(session_id.clone());
                created_session_id = Some(session_id);
            }
            Action::Cancel => {
                // Cancel and clean up session
                if let Some(ref session_id) = ctx.command.session_id {
                    let session = self.sessions.read().await.get(session_id).cloned();
                    if !session_matches_request(session.as_ref(), node, from) {
                        return CommandResult::Error(XmppError::bad_request(Some(
                            "Session not found, expired, or owned by another requester".to_string(),
                        )));
                    }
                    self.sessions.write().await.remove(session_id);
                    debug!(
                        node = %node,
                        session_id = %session_id,
                        "Canceled command session"
                    );
                }
                return CommandResult::Canceled { notes: vec![] };
            }
            Action::Complete | Action::Next | Action::Prev => {
                if ctx.command.session_id.is_none() {
                    return CommandResult::Error(XmppError::bad_request(Some(
                        "Command sessionid is required for next, prev, and complete actions"
                            .to_string(),
                    )));
                }
                // Touch the session to update last_accessed
                if let Some(ref session_id) = ctx.command.session_id {
                    if let Some(session) = self.sessions.write().await.get_mut(session_id) {
                        if !session_matches_request(Some(session), node, from) {
                            return CommandResult::Error(XmppError::bad_request(Some(
                                "Session not found, expired, or owned by another requester"
                                    .to_string(),
                            )));
                        }
                        session.touch();
                    } else {
                        return CommandResult::Error(XmppError::bad_request(Some(
                            "Session not found or expired".to_string(),
                        )));
                    }
                }
            }
        }

        let active_session_id = ctx.command.session_id.clone();

        // Call the handler
        let result = match (created_session_id, handler(ctx).await) {
            (
                Some(session_id),
                CommandResult::Executing {
                    form,
                    session_id: response_session_id,
                    notes,
                },
            ) if response_session_id.is_empty() => CommandResult::Executing {
                form,
                session_id,
                notes,
            },
            (_, result) => result,
        };

        match &result {
            CommandResult::Executing { .. } => {}
            CommandResult::Completed { .. }
            | CommandResult::Canceled { .. }
            | CommandResult::Error(_) => {
                if let Some(session_id) = active_session_id {
                    self.sessions.write().await.remove(&session_id);
                }
            }
        }

        result
    }

    /// Get a command session by ID.
    pub async fn get_session(&self, session_id: &str) -> Option<CommandSession> {
        self.sessions.read().await.get(session_id).cloned()
    }

    /// Update a command session's state.
    pub async fn update_session_state(
        &self,
        session_id: &str,
        key: String,
        value: String,
    ) -> Result<(), ()> {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(session_id) {
            session.state.insert(key, value);
            Ok(())
        } else {
            Err(())
        }
    }

    /// Remove a command session.
    pub async fn remove_session(&self, session_id: &str) {
        self.sessions.write().await.remove(session_id);
    }

    /// Clean up expired sessions.
    async fn cleanup_expired_sessions(&self) {
        let mut sessions = self.sessions.write().await;
        let expired: Vec<String> = sessions
            .iter()
            .filter(|(_, session)| session.is_expired())
            .map(|(id, _)| id.clone())
            .collect();

        for id in expired {
            sessions.remove(&id);
            debug!(session_id = %id, "Cleaned up expired command session");
        }
    }
}

fn session_matches_request(session: Option<&CommandSession>, node: &str, from: &Jid) -> bool {
    session
        .filter(|session| !session.is_expired())
        .is_some_and(|session| session.node == node && session.from == *from)
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_list_commands() {
        let registry = CommandRegistry::new();

        registry
            .register(
                "test:command",
                "Test Command",
                |_ctx: CommandContext| async { CommandResult::Canceled { notes: vec![] } },
            )
            .await;

        let commands = registry.list_commands().await;
        assert_eq!(commands.len(), 1);
        assert!(commands.contains(&("test:command".to_string(), "Test Command".to_string())));
    }

    #[tokio::test]
    async fn test_session_expiry() {
        let session = CommandSession::new(
            "test-session".to_string(),
            "test:node".to_string(),
            "user@localhost".parse().unwrap(),
        );

        assert!(!session.is_expired());
    }

    #[tokio::test]
    async fn test_execute_threads_generated_session_id_to_handler() {
        let registry = CommandRegistry::new();

        registry
            .register(
                "test:command",
                "Test Command",
                |ctx: CommandContext| async move {
                    CommandResult::Executing {
                        form: DataForm::new(crate::xep::xep0004::FormType::Form),
                        session_id: ctx.command.session_id.unwrap_or_default(),
                        notes: vec![],
                    }
                },
            )
            .await;

        let result = registry
            .dispatch(CommandContext {
                from: "user@localhost".parse().unwrap(),
                authenticated_user_id: Some("user-123".to_string()),
                iq: Iq::Set {
                    from: None,
                    to: None,
                    id: "test-iq".to_string(),
                    payload: "<query xmlns='urn:test'/>".parse().unwrap(),
                },
                command: Command::new("test:command").with_action(Action::Execute),
            })
            .await;

        let session_id = match result {
            CommandResult::Executing { session_id, .. } => session_id,
            _ => panic!("expected executing result"),
        };

        assert!(!session_id.is_empty());
        assert!(registry.get_session(&session_id).await.is_some());
    }
}
