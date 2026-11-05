//! Test-only RFC 7395 pre-bind transcript with separate scripted and
//! session-validated authentication fixtures. Neither can create a binding.
use super::{
    ActorOutbound, ActorOutput, ConnectionActor, ConnectionRef, ConnectionWireFrame, Context,
    Message, QueueItem, Stanza, StopReason, TrackedQueueItem,
};
use crate::auth::{localpart_to_jid, Session};
use crate::server::routes::auth::AuthState;
use base64::Engine;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::auth::{
    parse_oauthbearer, AuthenticatedPrincipalRef, OAuthBearerCredentials, OAuthBearerParseError,
    OAuthBearerResult,
};
use waddle_xmpp::protocol::frame::{parse_frame_with_metadata, InboundFrame, ParseError};
use waddle_xmpp::stream_management::{SmFailed, SmStanza};
use xmpp_parsers::message::Lang;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stream_error::{DefinedCondition, StreamError};

/// Strictly parsed pre-bind input; raw SASL payloads never enter the mailbox.
#[derive(Clone)]
pub enum PrebindInput {
    Open(Option<jid::BareJid>),
    Close,
    Auth(PrebindSaslInput),
    Abort,
    UnsupportedContinuation,
    Stanza {
        stanza: Arc<Stanza>,
        message_stanza_lang: Option<Lang>,
    },
    StreamManagement(SmStanza),
    ParseError(Arc<ParseError>),
    NonText,
}

impl std::fmt::Debug for PrebindInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Open(_) => "Open",
            Self::Close => "Close",
            Self::Auth(_) => "Auth([redacted])",
            Self::Abort => "Abort",
            Self::UnsupportedContinuation => "UnsupportedContinuation",
            Self::Stanza { .. } => "Stanza",
            Self::StreamManagement(_) => "StreamManagement",
            Self::ParseError(_) => "ParseError",
            Self::NonText => "NonText",
        })
    }
}

/// SASL is classified at the transport boundary before entering the actor.
#[derive(Clone)]
pub enum PrebindSaslInput {
    OAuthBearer(OAuthBearerCredentials),
    IncorrectEncoding,
    Malformed,
    InvalidAuthzid,
    UnsupportedMechanism,
}

pub(super) fn parse_input(text: &str) -> PrebindInput {
    if SmStanza::is_client_nonza_candidate(text) {
        return SmStanza::parse(text)
            .map(PrebindInput::StreamManagement)
            .unwrap_or_else(|| {
                PrebindInput::ParseError(Arc::new(ParseError::InvalidXml(
                    "invalid stream-management element".into(),
                )))
            });
    }
    match parse_frame_with_metadata(text) {
        Ok(parsed) => match parsed.frame {
            InboundFrame::Open { from } => PrebindInput::Open(from),
            InboundFrame::Close => PrebindInput::Close,
            InboundFrame::Auth { mechanism, data } => {
                let auth = if mechanism != "OAUTHBEARER" {
                    PrebindSaslInput::UnsupportedMechanism
                } else {
                    match base64::engine::general_purpose::STANDARD.decode(data) {
                        Err(_) => PrebindSaslInput::IncorrectEncoding,
                        Ok(bytes) => match parse_oauthbearer(&bytes) {
                            Ok(OAuthBearerResult::Credentials(credentials)) => {
                                PrebindSaslInput::OAuthBearer(credentials)
                            }
                            Ok(OAuthBearerResult::DiscoveryRequest)
                            | Err(OAuthBearerParseError::Malformed) => PrebindSaslInput::Malformed,
                            Err(OAuthBearerParseError::InvalidAuthzid) => {
                                PrebindSaslInput::InvalidAuthzid
                            }
                        },
                    }
                };
                PrebindInput::Auth(auth)
            }
            InboundFrame::SaslResponse(_) => PrebindInput::UnsupportedContinuation,
            InboundFrame::Sasl2Authenticate(_) => {
                PrebindInput::Auth(PrebindSaslInput::UnsupportedMechanism)
            }
            InboundFrame::Sasl2Response(_) => PrebindInput::UnsupportedContinuation,
            InboundFrame::Stanza(stanza) => PrebindInput::Stanza {
                stanza: Arc::new(*stanza),
                message_stanza_lang: parsed.message_stanza_lang,
            },
        },
        Err(ParseError::UnknownRoot(root))
            if root == "abort"
                || root
                    .split_once(':')
                    .is_some_and(|(_, local)| local == "abort") =>
        {
            match text.parse::<Element>() {
                Ok(element)
                    if element.is("abort", waddle_xmpp::ns::SASL)
                        && element.children().next().is_none()
                        && element.text().is_empty() =>
                {
                    PrebindInput::Abort
                }
                _ => PrebindInput::ParseError(Arc::new(ParseError::InvalidXml(
                    "invalid SASL abort element".into(),
                ))),
            }
        }
        Err(error) => PrebindInput::ParseError(Arc::new(error)),
    }
}

/// Fixture-only phase; it does not alter the production session phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrebindPhase {
    AwaitInitialOpen,
    PreAuth,
    AuthPending,
    AwaitRestart,
    AuthenticatedUnbound,
    Closing,
}

/// An exact connection and attempt nonce; only the current token can complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrebindAuthToken {
    connection_ref: ConnectionRef,
    nonce: u64,
}

/// Parsed credentials offered to the scripted test authenticator.
pub struct PrebindAuthAttempt {
    pub token: PrebindAuthToken,
    pub credentials: OAuthBearerCredentials,
}

/// Scripted result; this never represents a production session.
pub enum PrebindAuthOutcome {
    Authenticated(jid::BareJid),
    Rejected,
}

/// Deliver a scripted result. Stale, foreign, and repeated tokens are ignored.
pub struct CompletePrebindAuth {
    pub token: PrebindAuthToken,
    pub outcome: PrebindAuthOutcome,
}

/// Actor-issued, immutable validation input for the fixture's own auth state.
/// It cannot be constructed or retargeted by the receiver.
pub struct PrebindValidationAttempt {
    token: PrebindAuthToken,
    credentials: OAuthBearerCredentials,
    auth_state: Arc<AuthState>,
    cancellation: CancellationToken,
}

#[derive(Clone)]
struct ValidatedSession {
    session: Session,
    identity: jid::BareJid,
}

/// Opaque result of validating one actor-issued attempt. The private token
/// prevents a validated result from being moved to another attempt.
///
/// The session is a snapshot from lookup time. Revocation, expiry, or an
/// authority change after lookup is not fenced against actor installation.
/// This fixture never binds or publishes the resulting identity. A live
/// cutover requires a shared revoke/install fence or authoritative
/// revalidation at later effect boundaries, as appropriate to its guarantees.
#[derive(Clone)]
pub struct CompleteValidatedPrebindAuth {
    token: PrebindAuthToken,
    session: Option<ValidatedSession>,
}

impl PrebindValidationAttempt {
    /// Whether replacement, abort, connection close, or actor retirement has
    /// cancelled this attempt.
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Validate using the same session lookup as the live OAUTHBEARER path.
    /// This fixture performs no last-used update, binding, or publication.
    /// Cancellation drops the lookup future and yields no completion. A
    /// successful lookup is not revalidated when its result is delivered.
    pub async fn validate(self) -> Option<CompleteValidatedPrebindAuth> {
        let Self {
            token,
            credentials,
            auth_state,
            cancellation,
        } = self;
        let session = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return None,
            result = auth_state.session_manager.validate_session_without_touch(&credentials.token) => result.ok(),
        };
        if cancellation.is_cancelled() {
            return None;
        }
        let session = session.and_then(|session| {
            let identity = session.user_jid.parse::<jid::BareJid>().ok()?;
            identity.node()?;
            let derived = localpart_to_jid(&session.xmpp_localpart, &auth_state.xmpp_domain)
                .ok()?
                .parse::<jid::BareJid>()
                .ok()?;
            (identity == derived).then_some(ValidatedSession { session, identity })
        });
        Some(CompleteValidatedPrebindAuth { token, session })
    }
}

/// Snapshot of the dark fixture's phase and pending scripted attempt.
#[derive(Debug, Clone, kameo::Reply)]
pub struct PrebindSnapshot {
    pub phase: PrebindPhase,
    pub pending_auth: Option<PrebindAuthToken>,
    pub output_pending: bool,
    /// Non-secret durable identity of the exact validated session, if installed.
    pub validated_principal: Option<AuthenticatedPrincipalRef>,
    /// Whether the actor-owned production phase holds an authenticated identity.
    pub state_authenticated: bool,
    /// Language metadata captured at the inbound stanza parsing boundary.
    pub last_stanza_language: Option<Lang>,
}
/// Read the dark fixture's current state.
pub struct SnapshotPrebind;
impl Message<SnapshotPrebind> for ConnectionActor {
    type Reply = Option<PrebindSnapshot>;
    async fn handle(
        &mut self,
        _: SnapshotPrebind,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.prebind.as_ref().map(|p| PrebindSnapshot {
            phase: p.phase,
            pending_auth: p.pending_auth.as_ref().map(|pending| pending.token.clone()),
            output_pending: p.pending_output.is_some(),
            validated_principal: self
                .core
                .state
                .authenticated_session
                .as_ref()
                .and_then(|session| session.authenticated_principal_ref().ok()),
            state_authenticated: self.core.state.phase.is_authenticated(),
            last_stanza_language: p.last_stanza_language.clone(),
        })
    }
}

pub(super) enum PrebindOutput {
    Element(Element),
    TransportClose,
}
pub(super) struct PrebindWriteCompleted {
    pub id: u64,
    pub progress: super::super::super::supervised_io::WriteProgress,
}
struct PendingOutput {
    id: u64,
    remaining: VecDeque<PrebindOutput>,
    next_phase: PrebindPhase,
}
enum Deferred {
    Input(TrackedQueueItem),
    Auth(Box<AuthCompletion>),
}

enum AuthCompletion {
    Scripted(CompletePrebindAuth),
    Validated(CompleteValidatedPrebindAuth),
}

impl AuthCompletion {
    fn token(&self) -> &PrebindAuthToken {
        match self {
            Self::Scripted(completion) => &completion.token,
            Self::Validated(completion) => &completion.token,
        }
    }

    fn identity(&self) -> Option<&jid::BareJid> {
        match self {
            Self::Scripted(CompletePrebindAuth {
                outcome: PrebindAuthOutcome::Authenticated(identity),
                ..
            }) => Some(identity),
            Self::Scripted(_) => None,
            Self::Validated(completion) => completion.session.as_ref().map(|s| &s.identity),
        }
    }
}

enum AuthAttempts {
    Scripted(mpsc::Sender<PrebindAuthAttempt>),
    Validated {
        attempts: mpsc::Sender<PrebindValidationAttempt>,
        auth_state: Arc<AuthState>,
    },
}

struct PendingAuth {
    token: PrebindAuthToken,
    authzid: Option<jid::BareJid>,
    cancellation: CancellationToken,
}

impl Drop for PendingAuth {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

pub(super) struct PrebindState {
    phase: PrebindPhase,
    domain: jid::BareJid,
    opening_from: Option<jid::BareJid>,
    current_open_from: Option<jid::BareJid>,
    identity: Option<jid::BareJid>,
    attempts: AuthAttempts,
    next_auth_nonce: u64,
    pending_auth: Option<PendingAuth>,
    next_output_id: u64,
    pending_output: Option<PendingOutput>,
    deferred: VecDeque<Deferred>,
    last_stanza_language: Option<Lang>,
}

impl PrebindState {
    pub(super) fn new(domain: jid::BareJid, attempts: mpsc::Sender<PrebindAuthAttempt>) -> Self {
        Self::with_attempts(domain, AuthAttempts::Scripted(attempts))
    }

    pub(super) fn new_validated(
        domain: jid::BareJid,
        attempts: mpsc::Sender<PrebindValidationAttempt>,
        auth_state: Arc<AuthState>,
    ) -> Self {
        Self::with_attempts(
            domain,
            AuthAttempts::Validated {
                attempts,
                auth_state,
            },
        )
    }

    fn with_attempts(domain: jid::BareJid, attempts: AuthAttempts) -> Self {
        Self {
            phase: PrebindPhase::AwaitInitialOpen,
            domain,
            opening_from: None,
            current_open_from: None,
            identity: None,
            attempts,
            next_auth_nonce: 1,
            pending_auth: None,
            next_output_id: 1,
            pending_output: None,
            deferred: VecDeque::new(),
            last_stanza_language: None,
        }
    }

    fn enqueue(&mut self, actor: &ConnectionActor, frame: PrebindOutput) -> Option<u64> {
        let id = self.next_output_id;
        let Some(next) = id.checked_add(1) else {
            actor.io_control.request_stop(StopReason::Requested);
            return None;
        };
        self.next_output_id = next;
        match &actor.outbound {
            ActorOutbound::WebSocket(_) => actor
                .enqueue_websocket_output(ActorOutput::Prebind { id, frame })
                .ok()
                .map(|()| id),
            ActorOutbound::Synthetic(_) => {
                actor.io_control.request_stop(StopReason::Requested);
                None
            }
        }
    }

    fn sequence(
        &mut self,
        actor: &ConnectionActor,
        mut frames: VecDeque<PrebindOutput>,
        next_phase: PrebindPhase,
    ) {
        let Some(first) = frames.pop_front() else {
            return;
        };
        if let Some(id) = self.enqueue(actor, first) {
            self.pending_output = Some(PendingOutput {
                id,
                remaining: frames,
                next_phase,
            });
        }
    }

    fn element(element: Element) -> PrebindOutput {
        PrebindOutput::Element(element)
    }
    fn open(&self) -> Element {
        super::super::super::transport_xml::websocket_stream_open_element(
            &self.domain.to_string(),
            self.current_open_from.as_ref(),
        )
    }
    fn features(authenticated: bool) -> Element {
        let mut features = Element::builder("features", waddle_xmpp::ns::STREAM);
        if !authenticated {
            features = features.append(
                Element::builder("mechanisms", waddle_xmpp::ns::SASL)
                    .append(
                        Element::builder("mechanism", waddle_xmpp::ns::SASL)
                            .append("OAUTHBEARER")
                            .build(),
                    )
                    .build(),
            );
        }
        features.build()
    }
    fn failure(condition: xmpp_parsers::sasl::DefinedCondition) -> Element {
        Element::builder("failure", waddle_xmpp::ns::SASL)
            .append(Element::from(condition))
            .build()
    }
    fn stream_error(condition: DefinedCondition) -> Element {
        Element::from(StreamError {
            condition,
            texts: Default::default(),
            application_specific: Vec::new(),
        })
    }
    fn close(
        &mut self,
        actor: &ConnectionActor,
        condition: Option<DefinedCondition>,
        opening: bool,
    ) {
        let mut frames = VecDeque::new();
        if opening {
            frames.push_back(Self::element(self.open()));
        }
        if let Some(condition) = condition {
            frames.push_back(Self::element(Self::stream_error(condition)));
        }
        frames.push_back(Self::element(
            super::super::super::transport_xml::websocket_stream_close_element(),
        ));
        frames.push_back(PrebindOutput::TransportClose);
        self.phase = PrebindPhase::Closing;
        self.pending_auth = None;
        self.sequence(actor, frames, PrebindPhase::Closing);
    }
    fn one(&mut self, actor: &ConnectionActor, element: Element, next: PrebindPhase) {
        self.sequence(actor, VecDeque::from([Self::element(element)]), next);
    }
    fn event(&mut self, actor: &mut ConnectionActor, event: Deferred) {
        if self.pending_output.is_some() {
            if let Deferred::Auth(result) = &event {
                let is_current = self.phase == PrebindPhase::AuthPending
                    && self
                        .pending_auth
                        .as_ref()
                        .is_some_and(|pending| pending.token == *result.token())
                    && result.token().connection_ref == *actor.core.connection_ref();
                if !is_current
                    || self
                        .deferred
                        .iter()
                        .any(|item| matches!(item, Deferred::Auth(_)))
                {
                    return;
                }
            }
            self.deferred.push_back(event);
            return;
        }
        match event {
            Deferred::Input(item) => self.input(actor, item),
            Deferred::Auth(result) => self.auth_result(actor, *result),
        }
    }
    fn input(&mut self, actor: &ConnectionActor, item: TrackedQueueItem) {
        let Some(QueueItem {
            value: ConnectionWireFrame::Prebind { input, .. },
            ..
        }) = item.item.as_ref()
        else {
            actor.io_control.request_stop(StopReason::Requested);
            return;
        };
        let opening = matches!(
            self.phase,
            PrebindPhase::AwaitInitialOpen | PrebindPhase::AwaitRestart
        );
        match input.as_ref() {
            PrebindInput::Open(from) if opening => {
                self.current_open_from = from.clone();
                if self.phase == PrebindPhase::AwaitRestart
                    && from
                        .as_ref()
                        .is_some_and(|from| self.identity.as_ref() != Some(from))
                {
                    self.close(actor, Some(DefinedCondition::InvalidFrom), true);
                    return;
                }
                if self.phase == PrebindPhase::AwaitInitialOpen {
                    self.opening_from = from.clone();
                }
                let authenticated = self.phase == PrebindPhase::AwaitRestart;
                let next = if authenticated {
                    PrebindPhase::AuthenticatedUnbound
                } else {
                    PrebindPhase::PreAuth
                };
                self.sequence(
                    actor,
                    VecDeque::from([
                        Self::element(self.open()),
                        Self::element(Self::features(authenticated)),
                    ]),
                    next,
                );
            }
            PrebindInput::ParseError(error) => {
                let condition = match error.as_ref() {
                    ParseError::InvalidOpenFrom => DefinedCondition::InvalidFrom,
                    ParseError::InvalidOpenNamespace => DefinedCondition::InvalidNamespace,
                    ParseError::InvalidOpenContent => DefinedCondition::InvalidXml,
                    ParseError::InvalidOpenXml | ParseError::InvalidXml(_) => {
                        DefinedCondition::NotWellFormed
                    }
                    ParseError::MalformedSasl(_) if !opening => {
                        if self.phase == PrebindPhase::AuthPending {
                            self.pending_auth = None;
                            self.phase = PrebindPhase::PreAuth;
                        }
                        self.one(
                            actor,
                            Self::failure(xmpp_parsers::sasl::DefinedCondition::InvalidMechanism),
                            self.phase,
                        );
                        return;
                    }
                    _ => DefinedCondition::BadFormat,
                };
                self.close(actor, Some(condition), opening);
            }
            PrebindInput::Close if !opening => self.close(actor, None, false),
            PrebindInput::Abort if self.phase == PrebindPhase::AuthPending => {
                self.pending_auth = None;
                self.phase = PrebindPhase::PreAuth;
                self.one(
                    actor,
                    Self::failure(xmpp_parsers::sasl::DefinedCondition::Aborted),
                    PrebindPhase::PreAuth,
                );
            }
            PrebindInput::Auth(auth)
                if matches!(
                    self.phase,
                    PrebindPhase::PreAuth | PrebindPhase::AuthPending
                ) =>
            {
                use xmpp_parsers::sasl::DefinedCondition as SaslCondition;
                // RFC 6120 section 6.4.2 replaces the ongoing exchange. A late
                // result for its old nonce can no longer authenticate this one.
                self.pending_auth = None;
                self.phase = PrebindPhase::PreAuth;
                let credentials = match auth {
                    PrebindSaslInput::OAuthBearer(credentials) => credentials,
                    PrebindSaslInput::IncorrectEncoding => {
                        self.one(
                            actor,
                            Self::failure(SaslCondition::IncorrectEncoding),
                            self.phase,
                        );
                        return;
                    }
                    PrebindSaslInput::Malformed => {
                        self.one(
                            actor,
                            Self::failure(SaslCondition::MalformedRequest),
                            self.phase,
                        );
                        return;
                    }
                    PrebindSaslInput::InvalidAuthzid => {
                        self.one(
                            actor,
                            Self::failure(SaslCondition::InvalidAuthzid),
                            self.phase,
                        );
                        return;
                    }
                    PrebindSaslInput::UnsupportedMechanism => {
                        self.one(
                            actor,
                            Self::failure(SaslCondition::InvalidMechanism),
                            self.phase,
                        );
                        return;
                    }
                };
                let nonce = self.next_auth_nonce;
                let Some(next) = nonce.checked_add(1) else {
                    actor.io_control.request_stop(StopReason::Requested);
                    return;
                };
                self.next_auth_nonce = next;
                let token = PrebindAuthToken {
                    connection_ref: actor.core.connection_ref().clone(),
                    nonce,
                };
                let cancellation = CancellationToken::new();
                self.pending_auth = Some(PendingAuth {
                    token: token.clone(),
                    authzid: credentials.authzid.clone(),
                    cancellation: cancellation.clone(),
                });
                self.phase = PrebindPhase::AuthPending;
                let admitted = match &self.attempts {
                    AuthAttempts::Scripted(attempts) => attempts
                        .try_send(PrebindAuthAttempt {
                            token,
                            credentials: credentials.clone(),
                        })
                        .is_ok(),
                    AuthAttempts::Validated {
                        attempts,
                        auth_state,
                    } => attempts
                        .try_send(PrebindValidationAttempt {
                            token,
                            credentials: credentials.clone(),
                            auth_state: Arc::clone(auth_state),
                            cancellation,
                        })
                        .is_ok(),
                };
                if !admitted {
                    actor.io_control.request_stop(StopReason::Requested);
                }
            }
            PrebindInput::StreamManagement(SmStanza::Enable(_) | SmStanza::Resume(_))
                if !opening && self.phase != PrebindPhase::Closing =>
            {
                self.one(
                    actor,
                    SmFailed::with_condition("unexpected-request").to_element(),
                    self.phase,
                );
            }
            PrebindInput::Stanza {
                message_stanza_lang,
                ..
            } if !opening => {
                self.last_stanza_language = message_stanza_lang.clone();
                self.close(actor, Some(DefinedCondition::NotAuthorized), false)
            }
            PrebindInput::UnsupportedContinuation if self.phase == PrebindPhase::PreAuth => self
                .one(
                    actor,
                    Self::failure(xmpp_parsers::sasl::DefinedCondition::MalformedRequest),
                    self.phase,
                ),
            _ => self.close(actor, Some(DefinedCondition::PolicyViolation), opening),
        }
    }
    fn auth_result(&mut self, actor: &mut ConnectionActor, result: AuthCompletion) {
        let Some(pending) = &self.pending_auth else {
            return;
        };
        if self.phase != PrebindPhase::AuthPending
            || pending.token != *result.token()
            || result.token().connection_ref != *actor.core.connection_ref()
        {
            return;
        }
        // Stop authority shares this gate. A completion that loses the race
        // to connection stop cannot install any connection state.
        let lifecycle_owner = Arc::clone(&actor.io_control.lifecycle);
        let Ok(lifecycle) = lifecycle_owner.lock_running() else {
            return;
        };
        let (invalid_authzid, invalid_opening_from) = match result.identity() {
            Some(identity) => (
                pending.authzid.as_ref().is_some_and(|id| id != identity),
                self.opening_from
                    .as_ref()
                    .is_some_and(|hint| hint != identity),
            ),
            None => (false, false),
        };
        self.pending_auth = None;
        let verified = match &result {
            AuthCompletion::Validated(completion) if !invalid_authzid && !invalid_opening_from => {
                completion.session.as_ref()
            }
            _ => None,
        };
        if let Some(session) = verified {
            actor.core.state.authenticated_session = Some(session.session.clone());
            actor.core.state.phase = waddle_xmpp::protocol::ConnectionPhase::Authenticated {
                bare_jid: session.identity.clone(),
            };
        }
        drop(lifecycle);
        if invalid_authzid {
            self.one(
                actor,
                Self::failure(xmpp_parsers::sasl::DefinedCondition::InvalidAuthzid),
                PrebindPhase::PreAuth,
            );
        } else if invalid_opening_from {
            self.phase = PrebindPhase::Closing;
            self.sequence(
                actor,
                VecDeque::from([
                    Self::element(Self::failure(
                        xmpp_parsers::sasl::DefinedCondition::NotAuthorized,
                    )),
                    Self::element(
                        super::super::super::transport_xml::websocket_stream_close_element(),
                    ),
                    PrebindOutput::TransportClose,
                ]),
                PrebindPhase::Closing,
            );
        } else {
            match result.identity() {
                Some(identity) => {
                    self.identity = Some(identity.clone());
                    self.current_open_from = None;
                    self.one(
                        actor,
                        super::super::super::transport_xml::sasl_success_element(),
                        PrebindPhase::AwaitRestart,
                    );
                }
                None => self.one(
                    actor,
                    Self::failure(xmpp_parsers::sasl::DefinedCondition::NotAuthorized),
                    PrebindPhase::PreAuth,
                ),
            }
        }
    }
    fn completed(&mut self, actor: &mut ConnectionActor, completed: PrebindWriteCompleted) {
        let Some(mut pending) = self.pending_output.take() else {
            return;
        };
        if pending.id != completed.id {
            self.pending_output = Some(pending);
            return;
        }
        if completed.progress != super::super::super::supervised_io::WriteProgress::Completed {
            actor.io_control.request_stop(StopReason::Requested);
            return;
        }
        if let Some(frame) = pending.remaining.pop_front() {
            if let Some(id) = self.enqueue(actor, frame) {
                pending.id = id;
                self.pending_output = Some(pending);
            }
            return;
        }
        self.phase = pending.next_phase;
        if self.phase == PrebindPhase::Closing {
            actor.io_control.request_stop(StopReason::Requested);
            return;
        }
        while self.pending_output.is_none() {
            let Some(event) = self.deferred.pop_front() else {
                break;
            };
            self.event(actor, event);
        }
    }
}

impl ConnectionActor {
    pub(super) fn handle_prebind_input(&mut self, item: TrackedQueueItem) {
        if let Some(mut state) = self.prebind.take() {
            state.event(self, Deferred::Input(item));
            self.prebind = Some(state);
        }
    }
}
impl Message<CompletePrebindAuth> for ConnectionActor {
    type Reply = ();
    async fn handle(&mut self, msg: CompletePrebindAuth, _: &mut Context<Self, Self::Reply>) {
        if let Some(mut state) = self.prebind.take() {
            if matches!(&state.attempts, AuthAttempts::Scripted(_)) {
                state.event(
                    self,
                    Deferred::Auth(Box::new(AuthCompletion::Scripted(msg))),
                );
            }
            self.prebind = Some(state);
        }
    }
}
impl Message<CompleteValidatedPrebindAuth> for ConnectionActor {
    type Reply = ();
    async fn handle(
        &mut self,
        msg: CompleteValidatedPrebindAuth,
        _: &mut Context<Self, Self::Reply>,
    ) {
        if let Some(mut state) = self.prebind.take() {
            if matches!(&state.attempts, AuthAttempts::Validated { .. }) {
                state.event(
                    self,
                    Deferred::Auth(Box::new(AuthCompletion::Validated(msg))),
                );
            }
            self.prebind = Some(state);
        }
    }
}
impl Message<PrebindWriteCompleted> for ConnectionActor {
    type Reply = ();
    async fn handle(&mut self, msg: PrebindWriteCompleted, _: &mut Context<Self, Self::Reply>) {
        if let Some(mut state) = self.prebind.take() {
            state.completed(self, msg);
            self.prebind = Some(state);
        }
    }
}
