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
use crate::xep::xep0050::{Action, AdHocCommandCondition, Command, Note};
use crate::XmppError;

/// Time-to-live for command sessions (5 minutes)
const SESSION_TTL: Duration = Duration::from_secs(300);
/// How long to remember timed-out session ids after cleanup.
const EXPIRED_SESSION_GRACE: Duration = Duration::from_secs(300);
/// Upper bound for the recently-expired session cache.
const MAX_RECENTLY_EXPIRED_SESSIONS: usize = 1024;

/// Result of executing a command handler.
pub enum CommandResult {
    /// Command is still executing, return a form for the next step.
    Executing {
        form: DataForm,
        session_id: String,
        notes: Vec<Note>,
        /// XEP-0050 §3 / §5.2 `<actions/>` element advertising which
        /// actions the requester may submit at the next stage. When
        /// `None`, no `<actions/>` is emitted (XEP-0050 §5.2 says the
        /// element MAY be omitted if `execute` defaults to `next`).
        /// Multi-step commands with a single follow-up SHOULD set this
        /// to `AllowedActions::new(Action::Complete).with_complete()`
        /// so generic XEP-0050 clients know `complete` finishes the
        /// wizard.
        actions: Option<crate::xep::xep0050::AllowedActions>,
    },
    /// Command completed successfully.
    Completed {
        session_id: Option<String>,
        form: Option<DataForm>,
        notes: Vec<Note>,
    },
    /// Command was canceled.
    Canceled {
        session_id: Option<String>,
        notes: Vec<Note>,
    },
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

struct ExpiredCommandSession {
    expired_at: Instant,
    node: String,
    from: Jid,
}

/// Registry for ad-hoc commands.
pub struct CommandRegistry {
    /// Registered command metadata by node
    commands: RwLock<HashMap<String, CommandMetadata>>,
    /// Active command sessions
    sessions: RwLock<HashMap<String, CommandSession>>,
    /// Recently timed-out sessions retained to distinguish
    /// `<session-expired/>` from unknown or foreign session ids.
    recently_expired_sessions: RwLock<HashMap<String, ExpiredCommandSession>>,
}

impl CommandRegistry {
    /// Create a new command registry.
    pub fn new() -> Self {
        Self {
            commands: RwLock::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
            recently_expired_sessions: RwLock::new(HashMap::new()),
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
                    // XEP-0050 §4.4: an unknown command node is
                    // `cancel`/`item-not-found` (no command-specific
                    // child), not `service-unavailable` (which §4.4
                    // reserves for "this JID does not support commands
                    // at all").
                    return CommandResult::Error(XmppError::item_not_found(Some(format!(
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
                    // §4.4 bad-action: the responder cannot accept an
                    // `execute` that carries an existing sessionid.
                    return CommandResult::Error(XmppError::ad_hoc_command(
                        AdHocCommandCondition::BadAction,
                        Some("execute must not include an existing sessionid".to_string()),
                    ));
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
                        return CommandResult::Error(
                            self.session_id_error(session_id, node, from).await,
                        );
                    }
                    self.sessions.write().await.remove(session_id);
                    debug!(
                        node = %node,
                        session_id = %session_id,
                        "Canceled command session"
                    );
                }
                return CommandResult::Canceled {
                    session_id: ctx.command.session_id,
                    notes: vec![],
                };
            }
            Action::Complete | Action::Next | Action::Prev => {
                if ctx.command.session_id.is_none() {
                    // §4.4 bad-action: next/prev/complete cannot be
                    // accepted without an established session.
                    return CommandResult::Error(XmppError::ad_hoc_command(
                        AdHocCommandCondition::BadAction,
                        Some(
                            "Command sessionid is required for next, prev, and complete actions"
                                .to_string(),
                        ),
                    ));
                }
                // Touch the session to update last_accessed
                if let Some(ref session_id) = ctx.command.session_id {
                    let session_matches = {
                        let mut sessions = self.sessions.write().await;
                        match sessions.get_mut(session_id) {
                            Some(session) if session_matches_request(Some(session), node, from) => {
                                session.touch();
                                true
                            }
                            Some(_) => false,
                            None => false,
                        }
                    };
                    if !session_matches {
                        return CommandResult::Error(
                            self.session_id_error(session_id, node, from).await,
                        );
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
                    actions,
                },
            ) if response_session_id.is_empty() => CommandResult::Executing {
                form,
                session_id,
                notes,
                actions,
            },
            (
                Some(session_id),
                CommandResult::Completed {
                    session_id: None,
                    form,
                    notes,
                },
            ) => CommandResult::Completed {
                session_id: Some(session_id),
                form,
                notes,
            },
            (
                Some(session_id),
                CommandResult::Canceled {
                    session_id: None,
                    notes,
                },
            ) => CommandResult::Canceled {
                session_id: Some(session_id),
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

        let mut expired_sessions = Vec::with_capacity(expired.len());
        for id in expired {
            if let Some(session) = sessions.remove(&id) {
                expired_sessions.push(session);
            }
            debug!(session_id = %id, "Cleaned up expired command session");
        }
        drop(sessions);

        self.record_expired_sessions(expired_sessions).await;
    }

    async fn record_expired_sessions(&self, sessions: Vec<CommandSession>) {
        if sessions.is_empty() {
            return;
        }

        let now = Instant::now();
        let mut recently_expired = self.recently_expired_sessions.write().await;
        prune_recently_expired_sessions(&mut recently_expired, now);

        for session in sessions {
            recently_expired.insert(
                session.id,
                ExpiredCommandSession {
                    expired_at: now,
                    node: session.node,
                    from: session.from,
                },
            );
        }

        cap_recently_expired_sessions(&mut recently_expired);
    }

    async fn recently_expired_session_matches_request(
        &self,
        session_id: &str,
        node: &str,
        from: &Jid,
    ) -> bool {
        let now = Instant::now();
        let mut recently_expired = self.recently_expired_sessions.write().await;
        prune_recently_expired_sessions(&mut recently_expired, now);

        recently_expired
            .get(session_id)
            .is_some_and(|session| session.node == node && session.from == *from)
    }

    async fn session_id_error(&self, session_id: &str, node: &str, from: &Jid) -> XmppError {
        if self
            .recently_expired_session_matches_request(session_id, node, from)
            .await
        {
            XmppError::ad_hoc_command(
                AdHocCommandCondition::SessionExpired,
                Some("Command session has expired".to_string()),
            )
        } else {
            XmppError::ad_hoc_command(
                AdHocCommandCondition::BadSessionId,
                Some("Session not found or owned by another requester".to_string()),
            )
        }
    }
}

fn prune_recently_expired_sessions(
    recently_expired: &mut HashMap<String, ExpiredCommandSession>,
    now: Instant,
) {
    recently_expired
        .retain(|_, session| now.duration_since(session.expired_at) <= EXPIRED_SESSION_GRACE);
}

fn cap_recently_expired_sessions(recently_expired: &mut HashMap<String, ExpiredCommandSession>) {
    let excess = recently_expired
        .len()
        .saturating_sub(MAX_RECENTLY_EXPIRED_SESSIONS);
    if excess == 0 {
        return;
    }

    let mut oldest: Vec<(String, Instant)> = recently_expired
        .iter()
        .map(|(id, session)| (id.clone(), session.expired_at))
        .collect();
    oldest.sort_by_key(|(_, expired_at)| *expired_at);

    for (id, _) in oldest.into_iter().take(excess) {
        recently_expired.remove(&id);
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
                |_ctx: CommandContext| async {
                    CommandResult::Canceled {
                        session_id: None,
                        notes: vec![],
                    }
                },
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
                        actions: None,
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

    #[tokio::test]
    async fn test_expired_session_returns_session_expired() {
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
                        actions: None,
                    }
                },
            )
            .await;

        let result = registry
            .dispatch(test_context(
                Command::new("test:command").with_action(Action::Execute),
            ))
            .await;
        let session_id = match result {
            CommandResult::Executing { session_id, .. } => session_id,
            _ => panic!("expected executing result"),
        };

        {
            let mut sessions = registry.sessions.write().await;
            let session = sessions
                .get_mut(&session_id)
                .expect("execute created a session");
            session.last_accessed = Instant::now() - SESSION_TTL - Duration::from_secs(1);
        }

        let result = registry
            .dispatch(test_context(
                Command::new("test:command")
                    .with_action(Action::Next)
                    .with_session_id(session_id),
            ))
            .await;

        match result {
            CommandResult::Error(XmppError::AdHocCommand { condition, .. }) => {
                assert_eq!(condition, AdHocCommandCondition::SessionExpired);
            }
            _ => panic!("expected session-expired error"),
        }
    }

    #[tokio::test]
    async fn test_unknown_session_returns_bad_sessionid() {
        let registry = CommandRegistry::new();

        registry
            .register(
                "test:command",
                "Test Command",
                |_ctx: CommandContext| async {
                    CommandResult::Completed {
                        session_id: None,
                        form: None,
                        notes: vec![],
                    }
                },
            )
            .await;

        let result = registry
            .dispatch(test_context(
                Command::new("test:command")
                    .with_action(Action::Complete)
                    .with_session_id("unknown-session"),
            ))
            .await;

        match result {
            CommandResult::Error(XmppError::AdHocCommand { condition, .. }) => {
                assert_eq!(condition, AdHocCommandCondition::BadSessionId);
            }
            _ => panic!("expected bad-sessionid error"),
        }
    }

    fn test_context(command: Command) -> CommandContext {
        CommandContext {
            from: "user@localhost".parse().expect("test jid"),
            authenticated_user_id: Some("user-123".to_string()),
            iq: Iq::Set {
                from: None,
                to: None,
                id: "test-iq".to_string(),
                payload: "<query xmlns='urn:test'/>".parse().expect("test payload"),
            },
            command,
        }
    }
}
