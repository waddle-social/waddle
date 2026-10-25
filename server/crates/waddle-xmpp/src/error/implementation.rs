//! Implementation behind the parent module facade.

use super::AdHocCommandCondition;
use super::CoreError;
use super::Entity;
use super::Error;

/// XMPP server errors.
#[derive(Debug, Error)]
pub enum XmppError {
    /// IO error (network, file)
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// TLS error
    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),

    /// XML parsing error
    #[error("XML parse error: {0}")]
    XmlParse(String),

    /// Authentication failed
    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    /// Session not found or expired
    #[error("Session not found or expired")]
    SessionNotFound,

    /// Permission denied
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// Resource conflict (e.g., duplicate resource binding)
    #[error("Resource conflict: {0}")]
    ResourceConflict(String),

    /// MUC error
    #[error("MUC error: {0}")]
    Muc(String),

    /// Stream error
    #[error("Stream error: {0}")]
    Stream(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Internal server error
    #[error("Internal error: {0}")]
    Internal(String),

    /// An exact durable ownership fence no longer authorizes work for the
    /// named entity. Kept distinct from backend failures so actor callers can
    /// seal and demote only on a definitive ownership result.
    #[error("exact ownership fence no longer authorizes {entity}")]
    OwnershipLost { entity: Entity },

    /// The durable store could not establish whether the exact ownership
    /// fence is still current. Callers must fail closed, but must not treat
    /// this as definitive proof that another owner won.
    #[error("exact ownership fence for {entity} is temporarily unavailable")]
    OwnershipUnavailable { entity: Entity },

    /// A fenced durable room mutation required the room's complete durable
    /// state row, but no such row exists. Keep this distinct from ownership
    /// loss and backend uncertainty so callers neither demote a live owner nor
    /// acknowledge state that cannot survive actor replacement.
    #[error("durable room state is missing for {entity}")]
    DurableRoomStateMissing { entity: Entity },

    /// Stanza error (for IQ error responses)
    #[error("Stanza error: {condition}")]
    Stanza {
        /// Error condition
        condition: StanzaErrorCondition,
        /// Error type
        error_type: StanzaErrorType,
        /// Optional text description
        text: Option<String>,
    },

    /// XEP-0060 §7.1.3.3 — a PubSub publish item arrived without the
    /// payload its node requires. Maps on the wire to
    /// `<bad-request/>` (type=`modify`) plus the pubsub-namespaced
    /// `<payload-required/>` extension element. Distinct from
    /// [`Self::Stanza`] with `BadRequest` so callers can route this
    /// case via typed match instead of stringly-typed text inspection
    /// (typed-payloads hard rule).
    #[error("PubSub publish missing required payload: {0:?}")]
    PubSubPayloadRequired(Option<String>),

    /// XEP-0060 §7.1.3.4 — a PubSub publish item carries a payload
    /// that fails the per-node payload-conformance contract (typed
    /// parser rejection, unknown attribute, etc.). Maps on the wire
    /// to `<bad-request/>` (type=`modify`) plus the pubsub-namespaced
    /// `<invalid-payload/>` extension element.
    #[error("PubSub publish invalid payload: {0:?}")]
    PubSubInvalidPayload(Option<String>),

    /// XEP-0050 §4.4 — an ad-hoc command failed with a command-specific
    /// error condition. Carries the typed
    /// [`AdHocCommandCondition`](crate::xep::xep0050::AdHocCommandCondition);
    /// the dispatch layer renders it as the mapped RFC 6120 stanza error
    /// (general condition + type) plus the command-namespaced
    /// specific-condition child (e.g.
    /// `<bad-sessionid xmlns='http://jabber.org/protocol/commands'/>`).
    /// Distinct from [`Self::Stanza`] so the §4.4 application condition is
    /// not flattened into stringly-typed `<text/>` (typed-payloads hard
    /// rule).
    #[error("ad-hoc command error: {condition:?}")]
    AdHocCommand {
        /// The XEP-0050 §4.4 specific condition.
        condition: AdHocCommandCondition,
        /// Optional human-readable diagnostic text.
        text: Option<String>,
    },
}

impl XmppError {
    /// Create a new XML parse error.
    pub fn xml_parse(msg: impl Into<String>) -> Self {
        Self::XmlParse(msg.into())
    }

    /// Create a new authentication error.
    pub fn auth_failed(msg: impl Into<String>) -> Self {
        Self::AuthFailed(msg.into())
    }

    /// Create a new permission denied error.
    pub fn permission_denied(msg: impl Into<String>) -> Self {
        Self::PermissionDenied(msg.into())
    }

    /// Create a new MUC error.
    pub fn muc(msg: impl Into<String>) -> Self {
        Self::Muc(msg.into())
    }

    /// Create a new stream error.
    pub fn stream(msg: impl Into<String>) -> Self {
        Self::Stream(msg.into())
    }

    /// Create a new configuration error.
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// Create a new internal error.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    /// Create a stanza error for 'not-authorized'.
    pub fn not_authorized(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::NotAuthorized,
            error_type: StanzaErrorType::Auth,
            text,
        }
    }

    /// Create a stanza error for 'bad-request'.
    pub fn bad_request(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::BadRequest,
            error_type: StanzaErrorType::Modify,
            text,
        }
    }

    /// Create a stanza error for 'item-not-found'.
    pub fn item_not_found(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::ItemNotFound,
            error_type: StanzaErrorType::Cancel,
            text,
        }
    }

    /// Create a stanza error for 'feature-not-implemented'.
    pub fn feature_not_implemented(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::FeatureNotImplemented,
            error_type: StanzaErrorType::Cancel,
            text,
        }
    }

    /// Create a stanza error for 'forbidden'.
    pub fn forbidden(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::Forbidden,
            error_type: StanzaErrorType::Auth,
            text,
        }
    }

    /// Create a stanza error for 'internal-server-error'.
    pub fn internal_server_error(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::InternalServerError,
            error_type: StanzaErrorType::Wait,
            text,
        }
    }

    /// Create a stanza error for 'service-unavailable'.
    pub fn service_unavailable(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::ServiceUnavailable,
            error_type: StanzaErrorType::Cancel,
            text,
        }
    }

    /// Create a stanza error for 'conflict'.
    pub fn conflict(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::Conflict,
            error_type: StanzaErrorType::Cancel,
            text,
        }
    }

    /// Create a stanza error for 'registration-required'.
    ///
    /// Per XEP-0045, this is used when a user attempts to join a
    /// members-only room without being on the member list.
    pub fn registration_required(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::RegistrationRequired,
            error_type: StanzaErrorType::Auth,
            text,
        }
    }

    /// Create a stanza error for 'not-acceptable'.
    ///
    /// Used when the data provided is not acceptable (e.g., invalid username
    /// format during XEP-0077 registration).
    pub fn not_acceptable(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::NotAcceptable,
            error_type: StanzaErrorType::Modify,
            text,
        }
    }

    /// Create a stanza error for 'not-allowed'.
    ///
    /// Used when the operation is not allowed (e.g., registration disabled).
    pub fn not_allowed(text: Option<String>) -> Self {
        Self::Stanza {
            condition: StanzaErrorCondition::NotAllowed,
            error_type: StanzaErrorType::Cancel,
            text,
        }
    }

    /// Create the XEP-0060 §7.1.3.3 `<payload-required/>` PubSub
    /// publish error. The dispatch layer maps this variant onto the
    /// wire as `<bad-request/>` + `<payload-required xmlns='http://jabber.org/protocol/pubsub#errors'/>`.
    pub fn pubsub_payload_required(text: Option<String>) -> Self {
        Self::PubSubPayloadRequired(text)
    }

    /// Create the XEP-0060 §7.1.3.4 `<invalid-payload/>` PubSub
    /// publish error. The dispatch layer maps this variant onto the
    /// wire as `<bad-request/>` + `<invalid-payload xmlns='http://jabber.org/protocol/pubsub#errors'/>`.
    pub fn pubsub_invalid_payload(text: Option<String>) -> Self {
        Self::PubSubInvalidPayload(text)
    }

    /// Create a XEP-0050 §4.4 command-specific error
    /// ([`Self::AdHocCommand`]).
    pub fn ad_hoc_command(condition: AdHocCommandCondition, text: Option<String>) -> Self {
        Self::AdHocCommand { condition, text }
    }
}

impl From<CoreError> for XmppError {
    fn from(value: CoreError) -> Self {
        match value {
            CoreError::BadRequest(text) => Self::bad_request(text),
            CoreError::NotAcceptable(text) => Self::not_acceptable(text),
            CoreError::NotImplemented => Self::feature_not_implemented(Some(
                "waddle-xmpp-core scaffold is not implemented yet".to_string(),
            )),
            other => Self::internal_server_error(Some(other.to_string())),
        }
    }
}

/// XMPP stanza error conditions (RFC 6120 Section 8.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StanzaErrorCondition {
    /// Bad request (malformed XML, etc.)
    BadRequest,
    /// Conflict (e.g., resource already bound)
    Conflict,
    /// Feature not implemented
    FeatureNotImplemented,
    /// Forbidden (permission denied)
    Forbidden,
    /// Gone (entity no longer available)
    Gone,
    /// Internal server error
    InternalServerError,
    /// Item not found
    ItemNotFound,
    /// JID malformed
    JidMalformed,
    /// Not acceptable
    NotAcceptable,
    /// Not allowed
    NotAllowed,
    /// Not authorized
    NotAuthorized,
    /// Policy violation
    PolicyViolation,
    /// Recipient unavailable
    RecipientUnavailable,
    /// Redirect
    Redirect,
    /// Registration required
    RegistrationRequired,
    /// Remote server not found
    RemoteServerNotFound,
    /// Remote server timeout
    RemoteServerTimeout,
    /// Resource constraint
    ResourceConstraint,
    /// Service unavailable
    ServiceUnavailable,
    /// Subscription required
    SubscriptionRequired,
    /// Undefined condition
    UndefinedCondition,
    /// Unexpected request
    UnexpectedRequest,
}

impl StanzaErrorCondition {
    /// Convert a typed parser condition into Waddle's closed error vocabulary.
    pub fn from_xmpp(condition: &xmpp_parsers::stanza_error::DefinedCondition) -> Self {
        use xmpp_parsers::stanza_error::DefinedCondition as D;
        match condition {
            D::BadRequest => Self::BadRequest,
            D::Conflict => Self::Conflict,
            D::FeatureNotImplemented => Self::FeatureNotImplemented,
            D::Forbidden => Self::Forbidden,
            D::Gone { .. } => Self::Gone,
            D::InternalServerError => Self::InternalServerError,
            D::ItemNotFound => Self::ItemNotFound,
            D::JidMalformed => Self::JidMalformed,
            D::NotAcceptable => Self::NotAcceptable,
            D::NotAllowed => Self::NotAllowed,
            D::NotAuthorized => Self::NotAuthorized,
            D::PolicyViolation => Self::PolicyViolation,
            D::RecipientUnavailable => Self::RecipientUnavailable,
            D::Redirect { .. } => Self::Redirect,
            D::RegistrationRequired => Self::RegistrationRequired,
            D::RemoteServerNotFound => Self::RemoteServerNotFound,
            D::RemoteServerTimeout => Self::RemoteServerTimeout,
            D::ResourceConstraint => Self::ResourceConstraint,
            D::ServiceUnavailable => Self::ServiceUnavailable,
            D::SubscriptionRequired => Self::SubscriptionRequired,
            D::UndefinedCondition => Self::UndefinedCondition,
            D::UnexpectedRequest => Self::UnexpectedRequest,
        }
    }

    /// Get the element name for this condition.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BadRequest => "bad-request",
            Self::Conflict => "conflict",
            Self::FeatureNotImplemented => "feature-not-implemented",
            Self::Forbidden => "forbidden",
            Self::Gone => "gone",
            Self::InternalServerError => "internal-server-error",
            Self::ItemNotFound => "item-not-found",
            Self::JidMalformed => "jid-malformed",
            Self::NotAcceptable => "not-acceptable",
            Self::NotAllowed => "not-allowed",
            Self::NotAuthorized => "not-authorized",
            Self::PolicyViolation => "policy-violation",
            Self::RecipientUnavailable => "recipient-unavailable",
            Self::Redirect => "redirect",
            Self::RegistrationRequired => "registration-required",
            Self::RemoteServerNotFound => "remote-server-not-found",
            Self::RemoteServerTimeout => "remote-server-timeout",
            Self::ResourceConstraint => "resource-constraint",
            Self::ServiceUnavailable => "service-unavailable",
            Self::SubscriptionRequired => "subscription-required",
            Self::UndefinedCondition => "undefined-condition",
            Self::UnexpectedRequest => "unexpected-request",
        }
    }
}

impl StanzaErrorCondition {
    /// Convert to the `xmpp_parsers` typed defined-condition for typed
    /// `<error>` serialization (no stringly-typed XML construction).
    pub fn to_xmpp(self) -> xmpp_parsers::stanza_error::DefinedCondition {
        use xmpp_parsers::stanza_error::DefinedCondition as D;
        match self {
            Self::BadRequest => D::BadRequest,
            Self::Conflict => D::Conflict,
            Self::FeatureNotImplemented => D::FeatureNotImplemented,
            Self::Forbidden => D::Forbidden,
            Self::Gone => D::Gone { new_address: None },
            Self::InternalServerError => D::InternalServerError,
            Self::ItemNotFound => D::ItemNotFound,
            Self::JidMalformed => D::JidMalformed,
            Self::NotAcceptable => D::NotAcceptable,
            Self::NotAllowed => D::NotAllowed,
            Self::NotAuthorized => D::NotAuthorized,
            Self::PolicyViolation => D::PolicyViolation,
            Self::RecipientUnavailable => D::RecipientUnavailable,
            Self::Redirect => D::Redirect { new_address: None },
            Self::RegistrationRequired => D::RegistrationRequired,
            Self::RemoteServerNotFound => D::RemoteServerNotFound,
            Self::RemoteServerTimeout => D::RemoteServerTimeout,
            Self::ResourceConstraint => D::ResourceConstraint,
            Self::ServiceUnavailable => D::ServiceUnavailable,
            Self::SubscriptionRequired => D::SubscriptionRequired,
            Self::UndefinedCondition => D::UndefinedCondition,
            Self::UnexpectedRequest => D::UnexpectedRequest,
        }
    }
}

impl std::fmt::Display for StanzaErrorCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// XMPP stanza error types (RFC 6120 Section 8.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StanzaErrorType {
    /// Retry after providing credentials
    Auth,
    /// Do not retry (unrecoverable error)
    Cancel,
    /// Retry after changing the data sent
    Modify,
    /// Retry after waiting (temporary error)
    Wait,
}

impl StanzaErrorType {
    /// Get the type attribute value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Cancel => "cancel",
            Self::Modify => "modify",
            Self::Wait => "wait",
        }
    }
}

impl StanzaErrorType {
    /// Convert to the `xmpp_parsers` typed error type for typed
    /// `<error>` serialization.
    pub fn to_xmpp(self) -> xmpp_parsers::stanza_error::ErrorType {
        use xmpp_parsers::stanza_error::ErrorType as E;
        match self {
            Self::Auth => E::Auth,
            Self::Cancel => E::Cancel,
            Self::Modify => E::Modify,
            Self::Wait => E::Wait,
        }
    }
}

impl std::fmt::Display for StanzaErrorType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Generate a stream error and close tag.
///
/// Stream errors are fatal and must be followed by closing the stream.
pub fn generate_stream_error(condition: &str, text: Option<&str>) -> String {
    let mut error = format!(
        "<stream:error><{} xmlns='urn:ietf:params:xml:ns:xmpp-streams'/>",
        condition
    );

    if let Some(t) = text {
        error.push_str(&format!(
            "<text xmlns='urn:ietf:params:xml:ns:xmpp-streams' xml:lang='en'>{}</text>",
            t
        ));
    }

    error.push_str("</stream:error></stream:stream>");
    error
}
