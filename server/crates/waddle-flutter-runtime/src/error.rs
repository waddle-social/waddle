use serde::Serialize;
use waddle_xmpp_client_ffi::WaddleError;

/// Stable, credential-free errors at the Flutter I/O boundary.
#[derive(Clone, Debug, Serialize, thiserror::Error)]
#[error("{message}")]
pub(crate) struct RuntimeError {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

impl RuntimeError {
    pub(crate) fn invalid(message: &'static str) -> Self {
        Self {
            code: "invalid_request",
            message,
        }
    }

    pub(crate) fn retired() -> Self {
        Self {
            code: "session_changed",
            message: "The client session changed",
        }
    }

    pub(crate) fn quiescence() -> Self {
        Self {
            code: "close_unconfirmed",
            message: "The previous transport has not finished closing",
        }
    }

    pub(crate) fn unavailable() -> Self {
        Self {
            code: "not_connected",
            message: "Connect before issuing client requests",
        }
    }

    pub(crate) fn overflow() -> Self {
        Self {
            code: "resync_required",
            message: "The event consumer fell behind; reconnect and refresh history",
        }
    }
}

impl From<WaddleError> for RuntimeError {
    fn from(error: WaddleError) -> Self {
        let (code, message) = match error {
            WaddleError::NotConnected => ("not_connected", "The client is not connected"),
            WaddleError::InvalidJid => ("invalid_jid", "A JID is invalid"),
            WaddleError::InvalidArgument => ("invalid_argument", "An argument is invalid"),
            WaddleError::InvalidSessionId => {
                ("invalid_session_id", "The call session ID is invalid")
            }
            WaddleError::Stanza { .. } => ("stanza_error", "The server refused the operation"),
            WaddleError::MalformedResponse => (
                "malformed_response",
                "The server returned an invalid response",
            ),
            WaddleError::Transport => ("transport_error", "The transport failed"),
            WaddleError::Timeout => ("timeout", "The operation timed out"),
            WaddleError::UntrustedReply => {
                ("untrusted_reply", "The server reply could not be trusted")
            }
        };
        Self { code, message }
    }
}
