//! Command Registry for XEP-0050 Ad-Hoc Commands
//!
//! Manages command registration, session state, and dispatching for ad-hoc commands.

use crate::xep::xep0050::AllowedActions;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use jid::Jid;
use tokio::sync::RwLock;
use tracing::debug;
use uuid::Uuid;
use waddle_broker_core::boot_clock::{BootClock, BootInstant, SystemBootClock};
use waddle_broker_core::identity::TenantId;
use xmpp_parsers::iq::Iq;

use crate::error::StanzaErrorType;
use crate::xep::xep0004::{DataForm, FormType};
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
        actions: Option<AllowedActions>,
    },
    /// Command completed successfully.
    Completed {
        session_id: Option<String>,
        form: Option<DataForm>,
        notes: Vec<Note>,
    },
    /// Command completed with a multi-item result form (XEP-0004 §3.4) that
    /// is one page of a larger set. The form cannot carry paging fields, so
    /// the continuation token travels as the XEP-0059 `<last/>` of a
    /// `<set/>` beside the form; it is absent on the final page.
    CompletedPage {
        session_id: Option<String>,
        form: DataForm,
        next_cursor: Option<String>,
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
    /// Canonical tenant scoping this command execution when tenant-scoped.
    pub tenant_id: Option<TenantId>,
    /// Active placement lease generation fencing this command execution.
    pub placement_generation: Option<u64>,
    /// Cancellation signal propagated to decoupled long-running handlers.
    pub cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    /// Progressive chunk stream channel for decoupled inference handlers.
    pub chunk_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
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
    /// When this session was created (suspend-aware boot clock)
    pub created_at: BootInstant,
    /// When this session was last accessed (suspend-aware boot clock)
    pub last_accessed: BootInstant,
    /// Command node identifier
    pub node: String,
    /// Requester JID
    pub from: Jid,
    /// Canonical tenant owning this session when tenant-scoped.
    pub tenant_id: Option<TenantId>,
    /// Placement lease generation active when this session was created.
    pub placement_generation: Option<u64>,
    /// Custom session state (opaque to the registry)
    pub state: HashMap<String, String>,
    /// Actions advertised by the responder at the current stage.
    pub allowed_actions: Option<AllowedActions>,
}

impl CommandSession {
    fn new(
        id: String,
        node: String,
        from: Jid,
        now: BootInstant,
        tenant_id: Option<TenantId>,
        placement_generation: Option<u64>,
    ) -> Self {
        Self {
            id,
            created_at: now,
            last_accessed: now,
            node,
            from,
            tenant_id,
            placement_generation,
            state: HashMap::new(),
            allowed_actions: None,
        }
    }

    fn touch(&mut self, now: BootInstant) {
        self.last_accessed = now;
    }

    fn is_expired(&self, now: BootInstant) -> bool {
        now.saturating_duration_since(self.last_accessed) > SESSION_TTL
    }
}

struct ExpiredCommandSession {
    expired_at: BootInstant,
    node: String,
    from: Jid,
    tenant_id: Option<TenantId>,
    placement_generation: Option<u64>,
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
    /// Canonical tenant bound to this command registry when tenant-scoped.
    tenant_id: Option<TenantId>,
    /// Active placement lease generation fencing sessions in this registry.
    placement_generation: RwLock<Option<u64>>,
    /// Suspend-aware boot clock for session TTLs.
    clock: Arc<dyn BootClock>,
}

impl CommandRegistry {
    /// Create a new command registry.
    pub fn new() -> Self {
        Self::new_inner(None, None, Arc::new(SystemBootClock))
    }

    /// Create a new command registry explicitly scoped to `tenant_id`.
    pub fn for_tenant(tenant_id: TenantId) -> Self {
        Self::new_inner(Some(tenant_id), None, Arc::new(SystemBootClock))
    }

    /// Create a tenant-scoped command registry with an injectable [`BootClock`]
    /// and optional initial placement lease generation.
    pub fn for_tenant_with_clock(
        tenant_id: TenantId,
        placement_generation: Option<u64>,
        clock: Arc<dyn BootClock>,
    ) -> Self {
        Self::new_inner(Some(tenant_id), placement_generation, clock)
    }

    fn new_inner(
        tenant_id: Option<TenantId>,
        placement_generation: Option<u64>,
        clock: Arc<dyn BootClock>,
    ) -> Self {
        Self {
            commands: RwLock::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
            recently_expired_sessions: RwLock::new(HashMap::new()),
            tenant_id,
            placement_generation: RwLock::new(placement_generation),
            clock,
        }
    }

    /// Canonical tenant bound to this command registry, if configured.
    pub fn tenant_id(&self) -> Option<&TenantId> {
        self.tenant_id.as_ref()
    }

    /// Update the active placement lease generation for this registry.
    /// Existing sessions created under an older generation will be rejected
    /// with `<session-expired/>`.
    pub async fn set_placement_generation(&self, generation: Option<u64>) {
        *self.placement_generation.write().await = generation;
    }

    /// Evict all active command sessions belonging to `tenant_id` (used during
    /// tenant quiesce, drain, or lease loss), recording them as expired so any
    /// subsequent continuation receives XEP-0050 `<session-expired/>`.
    pub async fn evict_tenant(&self, tenant_id: &TenantId) -> usize {
        let mut sessions = self.sessions.write().await;
        let to_evict: Vec<String> = sessions
            .iter()
            .filter(|(_, session)| session.tenant_id.as_ref() == Some(tenant_id))
            .map(|(id, _)| id.clone())
            .collect();
        let mut evicted = Vec::with_capacity(to_evict.len());
        for id in to_evict {
            if let Some(session) = sessions.remove(&id) {
                evicted.push(session);
            }
        }
        drop(sessions);
        let count = evicted.len();
        self.record_expired_sessions(evicted).await;
        count
    }

    fn now_boot(&self) -> BootInstant {
        self.clock
            .now()
            .unwrap_or_else(|| BootInstant::from_raw_duration(Duration::ZERO))
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
        // Fail closed if this registry is tenant-scoped and the request carries
        // a missing or mismatched tenant_id.
        if let Some(expected_tenant) = &self.tenant_id {
            if ctx.tenant_id.as_ref() != Some(expected_tenant) {
                return CommandResult::Error(XmppError::forbidden(Some(
                    "command tenant mismatch".to_string(),
                )));
            }
        }
        let active_gen = *self.placement_generation.read().await;
        if let Some(expected_gen) = active_gen {
            if ctx.placement_generation.is_some_and(|g| g != expected_gen) {
                return CommandResult::Error(XmppError::ad_hoc_command(
                    AdHocCommandCondition::SessionExpired,
                    Some("Command placement generation mismatch".to_string()),
                ));
            }
        }
        let effective_tenant = self.tenant_id.clone().or_else(|| ctx.tenant_id.clone());
        let effective_gen = active_gen.or(ctx.placement_generation);
        let now = self.now_boot();

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

        // XEP-0050 §2.4: an absent `action` is `execute`. On a continuing
        // session `execute` is equivalent to the `execute` attribute of the
        // `<actions/>` the responder last advertised (default `next`).
        let mut action = ctx.command.action.unwrap_or(Action::Execute);
        if action == Action::Execute {
            if let Some(ref session_id) = ctx.command.session_id {
                let session = self.sessions.read().await.get(session_id).cloned();
                if !session_matches_request(
                    session.as_ref(),
                    node,
                    from,
                    effective_tenant.as_ref(),
                    effective_gen,
                    now,
                ) {
                    return CommandResult::Error(
                        self.session_id_error(
                            session_id,
                            node,
                            from,
                            effective_tenant.as_ref(),
                            effective_gen,
                        )
                        .await,
                    );
                }
                let allowed = session.and_then(|session| session.allowed_actions);
                action = match &allowed {
                    Some(actions) => actions.execute_default,
                    None => Action::Next,
                };
                if allowed.is_some_and(|actions| !actions.permits(action)) {
                    return CommandResult::Error(XmppError::ad_hoc_command(
                        AdHocCommandCondition::BadAction,
                        Some("The default execute action is not allowed at this stage".into()),
                    ));
                }
                ctx.command.action = Some(action);
            }
        }
        let mut created_session_id = None;
        match action {
            Action::Execute => {
                // `execute` carrying a sessionid was resolved above, so
                // this is a new command session.
                if ctx.command.session_id.is_some() {
                    return CommandResult::Error(XmppError::ad_hoc_command(
                        AdHocCommandCondition::BadAction,
                        Some("execute resolved to execute on an existing session".into()),
                    ));
                }
                // Create a new session
                let session_id = Uuid::new_v4().to_string();
                let session = CommandSession::new(
                    session_id.clone(),
                    node.clone(),
                    from.clone(),
                    now,
                    effective_tenant.clone(),
                    effective_gen,
                );
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
                    if !session_matches_request(
                        session.as_ref(),
                        node,
                        from,
                        effective_tenant.as_ref(),
                        effective_gen,
                        now,
                    ) {
                        return CommandResult::Error(
                            self.session_id_error(
                                session_id,
                                node,
                                from,
                                effective_tenant.as_ref(),
                                effective_gen,
                            )
                            .await,
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
                            Some(session)
                                if session_matches_request(
                                    Some(session),
                                    node,
                                    from,
                                    effective_tenant.as_ref(),
                                    effective_gen,
                                    now,
                                ) =>
                            {
                                session.touch(now);
                                true
                            }
                            Some(session)
                                if session.node == *node
                                    && session.from == *from
                                    && session.tenant_id.as_ref() == effective_tenant.as_ref()
                                    && session.placement_generation != effective_gen =>
                            {
                                // Placement generation advanced: evict stale session
                                // so caller receives `<session-expired/>`.
                                let evicted = sessions.remove(session_id);
                                drop(sessions);
                                if let Some(s) = evicted {
                                    self.record_expired_sessions(vec![s]).await;
                                }
                                false
                            }
                            Some(_) | None => false,
                        }
                    };
                    if !session_matches {
                        return CommandResult::Error(
                            self.session_id_error(
                                session_id,
                                node,
                                from,
                                effective_tenant.as_ref(),
                                effective_gen,
                            )
                            .await,
                        );
                    }
                }
            }
        }

        let active_session_id = ctx.command.session_id.clone();
        let continuing_session = created_session_id.is_none();

        // XEP-0050 §2.4: "Responders SHOULD consider any <x type='cancel'/>
        // to be <x type='submit'/>."
        if let Some(form) = ctx.command.form.as_mut() {
            if form.form_type == FormType::Cancel {
                form.form_type = FormType::Submit;
            }
        }

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
            CommandResult::Executing { actions, .. } => {
                if let Some(session_id) = active_session_id {
                    if let Some(session) = self.sessions.write().await.get_mut(&session_id) {
                        session.allowed_actions = actions.clone();
                    }
                }
            }
            // XEP-0050 §2.4: the session ends on completion or cancel. A
            // correctable (`modify`) error on a continuing session leaves it
            // open so the requester can resubmit with the same sessionid.
            CommandResult::Error(error) if continuing_session && is_correctable(error) => {}
            CommandResult::Completed { .. }
            | CommandResult::CompletedPage { .. }
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
        let now = self.now_boot();
        let mut sessions = self.sessions.write().await;
        let expired: Vec<String> = sessions
            .iter()
            .filter(|(_, session)| session.is_expired(now))
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

        let now = self.now_boot();
        let mut recently_expired = self.recently_expired_sessions.write().await;
        prune_recently_expired_sessions(&mut recently_expired, now);

        for session in sessions {
            recently_expired.insert(
                session.id,
                ExpiredCommandSession {
                    expired_at: now,
                    node: session.node,
                    from: session.from,
                    tenant_id: session.tenant_id,
                    placement_generation: session.placement_generation,
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
        tenant_id: Option<&TenantId>,
        placement_generation: Option<u64>,
    ) -> bool {
        let now = self.now_boot();
        let mut recently_expired = self.recently_expired_sessions.write().await;
        prune_recently_expired_sessions(&mut recently_expired, now);

        recently_expired.get(session_id).is_some_and(|session| {
            let gen_ok = match (session.placement_generation, placement_generation) {
                (Some(old_g), Some(cur_g)) => old_g <= cur_g,
                (None, None) => true,
                _ => false,
            };
            session.node == node
                && session.from == *from
                && session.tenant_id.as_ref() == tenant_id
                && gen_ok
        })
    }

    async fn session_id_error(
        &self,
        session_id: &str,
        node: &str,
        from: &Jid,
        tenant_id: Option<&TenantId>,
        placement_generation: Option<u64>,
    ) -> XmppError {
        if self
            .recently_expired_session_matches_request(
                session_id,
                node,
                from,
                tenant_id,
                placement_generation,
            )
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

/// Whether the requester can fix the request and retry within the session.
fn is_correctable(error: &XmppError) -> bool {
    match error {
        XmppError::Stanza { error_type, .. } => *error_type == StanzaErrorType::Modify,
        XmppError::AdHocCommand { condition, .. } => {
            condition.stanza_error().0 == StanzaErrorType::Modify
        }
        _ => false,
    }
}

fn prune_recently_expired_sessions(
    recently_expired: &mut HashMap<String, ExpiredCommandSession>,
    now: BootInstant,
) {
    recently_expired.retain(|_, session| {
        now.saturating_duration_since(session.expired_at) <= EXPIRED_SESSION_GRACE
    });
}

fn cap_recently_expired_sessions(recently_expired: &mut HashMap<String, ExpiredCommandSession>) {
    let excess = recently_expired
        .len()
        .saturating_sub(MAX_RECENTLY_EXPIRED_SESSIONS);
    if excess == 0 {
        return;
    }

    let mut oldest: Vec<(String, BootInstant)> = recently_expired
        .iter()
        .map(|(id, session)| (id.clone(), session.expired_at))
        .collect();
    oldest.sort_by_key(|(_, expired_at)| *expired_at);

    for (id, _) in oldest.into_iter().take(excess) {
        recently_expired.remove(&id);
    }
}

fn session_matches_request(
    session: Option<&CommandSession>,
    node: &str,
    from: &Jid,
    tenant_id: Option<&TenantId>,
    placement_generation: Option<u64>,
    now: BootInstant,
) -> bool {
    session
        .filter(|session| !session.is_expired(now))
        .is_some_and(|session| {
            session.node == node
                && session.from == *from
                && session.tenant_id.as_ref() == tenant_id
                && session.placement_generation == placement_generation
        })
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
        let now = BootInstant::from_raw_duration(Duration::from_secs(10));
        let session = CommandSession::new(
            "test-session".to_string(),
            "test:node".to_string(),
            "user@localhost".parse().unwrap(),
            now,
            None,
            None,
        );

        assert!(!session.is_expired(now));
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
                tenant_id: None,
                placement_generation: None,
                cancel_rx: None,
                chunk_tx: None,
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
        let clock = Arc::new(waddle_broker_core::boot_clock::FakeBootClock::new(
            Duration::from_secs(1_000),
        ));
        let registry = CommandRegistry::new_inner(None, None, clock.clone());

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

        // Simulate host suspend past SESSION_TTL using suspend-aware BootClock.
        clock.advance_suspend(SESSION_TTL + Duration::from_secs(1));

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

    #[tokio::test]
    async fn test_tenant_isolation_eviction_and_placement_generation_fencing() {
        let tenant_a = TenantId::parse("tenant-a.example").expect("valid");
        let tenant_b = TenantId::parse("tenant-b.example").expect("valid");
        let clock = Arc::new(waddle_broker_core::boot_clock::FakeBootClock::new(
            Duration::from_secs(100),
        ));
        let registry = CommandRegistry::for_tenant_with_clock(tenant_a.clone(), Some(1), clock);

        registry
            .register("test:wizard", "Wizard", |ctx: CommandContext| async move {
                CommandResult::Executing {
                    form: DataForm::new(crate::xep::xep0004::FormType::Form),
                    session_id: ctx.command.session_id.unwrap_or_default(),
                    notes: vec![],
                    actions: None,
                }
            })
            .await;

        // Cross-tenant dispatch rejected with forbidden.
        let mut wrong_tenant_ctx =
            test_context(Command::new("test:wizard").with_action(Action::Execute));
        wrong_tenant_ctx.tenant_id = Some(tenant_b);
        wrong_tenant_ctx.placement_generation = Some(1);
        assert!(matches!(
            registry.dispatch(wrong_tenant_ctx).await,
            CommandResult::Error(XmppError::Stanza { .. })
        ));

        // Tenant A starts wizard under generation 1.
        let mut ctx_a = test_context(Command::new("test:wizard").with_action(Action::Execute));
        ctx_a.tenant_id = Some(tenant_a.clone());
        ctx_a.placement_generation = Some(1);
        let session_id = match registry.dispatch(ctx_a).await {
            CommandResult::Executing { session_id, .. } => session_id,
            _ => panic!("expected executing"),
        };

        // Placement generation advances to 2 -> continuing gen-1 session returns <session-expired/>.
        registry.set_placement_generation(Some(2)).await;
        let mut next_ctx = test_context(
            Command::new("test:wizard")
                .with_action(Action::Next)
                .with_session_id(session_id),
        );
        next_ctx.tenant_id = Some(tenant_a.clone());
        next_ctx.placement_generation = Some(2);
        match registry.dispatch(next_ctx).await {
            CommandResult::Error(XmppError::AdHocCommand { condition, .. }) => {
                assert_eq!(condition, AdHocCommandCondition::SessionExpired);
            }
            _ => panic!("expected session-expired after placement generation bump"),
        }

        // Start a fresh session at gen 2, then evict_tenant -> subsequent step returns <session-expired/>.
        let mut ctx_a2 = test_context(Command::new("test:wizard").with_action(Action::Execute));
        ctx_a2.tenant_id = Some(tenant_a.clone());
        ctx_a2.placement_generation = Some(2);
        let session_id2 = match registry.dispatch(ctx_a2).await {
            CommandResult::Executing { session_id, .. } => session_id,
            _ => panic!("expected executing"),
        };
        assert_eq!(registry.evict_tenant(&tenant_a).await, 1);

        let mut after_evict = test_context(
            Command::new("test:wizard")
                .with_action(Action::Next)
                .with_session_id(session_id2),
        );
        after_evict.tenant_id = Some(tenant_a);
        after_evict.placement_generation = Some(2);
        match registry.dispatch(after_evict).await {
            CommandResult::Error(XmppError::AdHocCommand { condition, .. }) => {
                assert_eq!(condition, AdHocCommandCondition::SessionExpired);
            }
            _ => panic!("expected session-expired after evict_tenant"),
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
            tenant_id: None,
            placement_generation: None,
            cancel_rx: None,
            chunk_tx: None,
        }
    }
}
