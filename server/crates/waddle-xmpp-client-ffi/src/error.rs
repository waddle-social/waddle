use waddle_xmpp_client::ClientError;

/// Typed FFI error thrown by the fetch-style methods
/// (`fetch_mds_displayed`, `fetch_room_pins`). Send-style methods keep
/// their `bool` / [`crate::WaddleSendMessageOutcome`] shapes so the
/// existing Swift call sites stay untouched; only new `Result`-returning
/// surfaces use this enum.
#[derive(uniffi::Error, thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum WaddleError {
    /// No live session — connect first.
    #[error("not connected")]
    NotConnected,
    /// A caller-supplied JID failed to parse.
    #[error("invalid JID")]
    InvalidJid,
    /// A caller-supplied Jingle session id was empty or whitespace.
    #[error("invalid session id")]
    InvalidSessionId,
    /// A caller-supplied argument (pack id, pack content, hash
    /// algorithm) was empty, malformed, or unsupported.
    #[error("invalid argument")]
    InvalidArgument,
    /// The server answered with an RFC 6120 §8.3 stanza error.
    /// `condition` is the defined-condition element name (e.g.
    /// `forbidden`); `text` is the optional human-readable `<text/>`.
    #[error("stanza error: {condition}")]
    Stanza {
        condition: String,
        text: Option<String>,
    },
    /// The server answered success but the payload did not match the
    /// expected typed shape (server-side bug or schema drift).
    #[error("malformed server response")]
    MalformedResponse,
    /// The websocket transport failed or closed mid-request.
    #[error("transport failure")]
    Transport,
    /// The request timed out before the server replied.
    #[error("request timed out")]
    Timeout,
    /// The reply's stamped `from` does not match the queried entity
    /// (RFC 6120 §8.1.2.1 defense-in-depth); the payload was
    /// discarded rather than trusted.
    #[error("reply from an unexpected sender")]
    UntrustedReply,
}

/// Collapse the shared client error into the typed FFI error. Stanza
/// errors keep their condition/text; every other failure folds into
/// the coarse `NotConnected` / `Timeout` / `Transport` buckets — the
/// Swift caller retries or surfaces a generic failure either way, and
/// the detailed diagnostic still reaches the listener via `emit_error`
/// on the paths that report it.
pub(crate) fn client_error_to_waddle(error: &ClientError) -> WaddleError {
    match error {
        ClientError::Disconnected => WaddleError::NotConnected,
        ClientError::StanzaError(err) => WaddleError::Stanza {
            condition: err.condition.clone(),
            text: err.text.clone(),
        },
        ClientError::WebSocketConnectTimeout { .. } | ClientError::IqTimeout { .. } => {
            WaddleError::Timeout
        }
        _ => WaddleError::Transport,
    }
}
