use waddle_xmpp_client::ClientError;

use crate::WaddleSendMessageOutcome;

pub(crate) fn send_failure_outcome(error: &ClientError) -> WaddleSendMessageOutcome {
    match error {
        ClientError::Disconnected => WaddleSendMessageOutcome::NotConnected,
        ClientError::StanzaError(_) => WaddleSendMessageOutcome::StanzaError,
        ClientError::WebSocketConnectTimeout { .. }
        | ClientError::TransportClosed
        | ClientError::EmptyTransportFrame
        | ClientError::TransportFrameTooLarge { .. }
        | ClientError::InvalidTransportFrame
        | ClientError::UnsupportedWebSocketMessage => WaddleSendMessageOutcome::TransportError,
        ClientError::InvalidWebSocketRequest(_)
        | ClientError::WebSocket(_)
        | ClientError::WebSocketTlsConfig(_) => WaddleSendMessageOutcome::TransportError,
        _ => WaddleSendMessageOutcome::Error,
    }
}
