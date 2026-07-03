use super::*;

/// Upper bound for a single WebSocket send (including the flush that
/// `SinkExt::send` forces through to the socket).
///
/// A peer whose TCP path has black-holed (NAT expiry, network
/// partition — no RST/FIN) stops draining its receive window; once the
/// kernel send buffer fills, an unbounded `send().await` parks the
/// connection task inside a `select!` arm body, and the RFC 7395
/// keepalive timer arm (issue #1090) is never polled again — dead-peer
/// detection would silently degrade to the ~15-minute kernel TCP
/// timeout exactly when a busy outbound stream meets a dead peer. A
/// healthy client drains a single frame in milliseconds; 60s of
/// undrained buffer means the path is gone. Stalled sends report
/// failure, which every caller already treats as fatal teardown.
const SEND_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub(super) async fn send_ws_text_frames<S, E, I>(
    sender: &mut S,
    frames: I,
    failure_message: &'static str,
) -> bool
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
    I: IntoIterator<Item = String>,
{
    for frame in frames {
        debug!(len = frame.len(), "Sending XMPP WebSocket response");
        if !send_ws_message(sender, Message::Text(frame.into()), failure_message).await {
            return false;
        }
    }

    true
}

pub(super) async fn send_ws_message<S, E>(
    sender: &mut S,
    message: Message,
    failure_message: &'static str,
) -> bool
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    match tokio::time::timeout(SEND_STALL_TIMEOUT, sender.send(message)).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            error!(error = %error, "{failure_message}");
            false
        }
        Err(_elapsed) => {
            error!(
                stall_timeout_secs = SEND_STALL_TIMEOUT.as_secs(),
                "WebSocket send stalled past the write budget (peer not draining); \
                 treating connection as dead: {failure_message}"
            );
            false
        }
    }
}

pub(super) async fn close_ws_connection<S, E>(sender: &mut S, failure_message: &'static str) -> bool
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    match tokio::time::timeout(SEND_STALL_TIMEOUT, sender.close()).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            error!(error = %error, "{failure_message}");
            false
        }
        Err(_elapsed) => {
            error!(
                stall_timeout_secs = SEND_STALL_TIMEOUT.as_secs(),
                "WebSocket close stalled past the write budget: {failure_message}"
            );
            false
        }
    }
}
