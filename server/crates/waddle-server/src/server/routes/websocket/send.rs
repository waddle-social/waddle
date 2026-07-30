use super::*;

/// Result of a WebSocket write that is bound to one admitted node generation.
///
/// `start_send` is the transport commit point: once it has run under a valid
/// permit, a later lifecycle transition cannot retract the frame from the
/// sink. Before that point, revocation must suppress the write entirely.
pub(super) enum AuthoritySendOutcome {
    Sent,
    TransportClosed,
    AuthorityRevoked,
}

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

/// Send a sequence of XMPP text frames while the admitting generation is
/// authoritative.
///
/// This is for a typed XMPP control exchange that must not be committed to a
/// superseded socket. A revocation between frames deliberately suppresses the
/// remaining frame(s), so the old generation cannot commit any additional
/// control frame after it has lost authority.
pub(super) async fn send_ws_text_frames_with_authority<S, E, I>(
    sender: &mut S,
    frames: I,
    failure_message: &'static str,
    authority: (
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    ),
) -> AuthoritySendOutcome
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
    I: IntoIterator<Item = String>,
{
    for frame in frames {
        debug!(
            len = frame.len(),
            "Sending authority-bound XMPP WebSocket response"
        );
        match send_ws_message_with_authority(
            sender,
            Message::Text(frame.into()),
            failure_message,
            Some(authority),
        )
        .await
        {
            AuthoritySendOutcome::Sent => {}
            outcome => return outcome,
        }
    }

    AuthoritySendOutcome::Sent
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

/// Send one frame only while the admitting node generation remains
/// authoritative.
///
/// A `SinkExt::send` future combines readiness, `start_send`, and flushing.
/// That is too coarse for a generation fence: a revocation can occur while
/// `poll_ready` is parked, and a later poll of that future would otherwise run
/// `start_send` before the caller can observe cancellation. Keep readiness
/// cancellable, then revalidate immediately before the synchronous
/// `start_send` commit. Once committed, finish flushing normally; the frame
/// was already legitimately accepted by the transport.
pub(super) async fn send_ws_message_with_authority<S, E>(
    sender: &mut S,
    message: Message,
    failure_message: &'static str,
    authority: Option<(
        &crate::clustering::NodeAdmissionPermit,
        &tokio_util::sync::CancellationToken,
    )>,
) -> AuthoritySendOutcome
where
    S: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    let authoritative = || {
        authority.is_none_or(|(permit, shutdown)| {
            !shutdown.is_cancelled() && permit.revalidate().is_ok()
        })
    };
    if !authoritative() {
        return AuthoritySendOutcome::AuthorityRevoked;
    }

    // `futures::SinkExt` does not expose a portable `ready` future for every
    // Sink version we support. Poll the required `Sink::poll_ready` directly
    // so cancellation remains selectable while transport readiness is parked.
    let ready_result = {
        let ready = std::future::poll_fn(|cx| std::pin::Pin::new(&mut *sender).poll_ready(cx));
        tokio::pin!(ready);
        match authority {
            Some((permit, shutdown)) => tokio::select! {
                biased;
                _ = shutdown.cancelled() => return AuthoritySendOutcome::AuthorityRevoked,
                _ = permit.revoked() => return AuthoritySendOutcome::AuthorityRevoked,
                result = tokio::time::timeout(SEND_STALL_TIMEOUT, ready.as_mut()) => result,
            },
            None => tokio::time::timeout(SEND_STALL_TIMEOUT, ready.as_mut()).await,
        }
    };
    match ready_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            error!(error = %error, "{failure_message}");
            return AuthoritySendOutcome::TransportClosed;
        }
        Err(_elapsed) => {
            error!(
                stall_timeout_secs = SEND_STALL_TIMEOUT.as_secs(),
                "WebSocket send stalled past the write budget (peer not draining); \\
                 treating connection as dead: {failure_message}"
            );
            return AuthoritySendOutcome::TransportClosed;
        }
    }
    // `poll_ready` may have parked while the generation was revoked, then
    // returned Ready in the same poll that wakes this task. This is the last
    // point at which the pending frame can still be suppressed.
    if !authoritative() {
        return AuthoritySendOutcome::AuthorityRevoked;
    }
    if let Err(error) = std::pin::Pin::new(&mut *sender).start_send(message) {
        error!(error = %error, "{failure_message}");
        return AuthoritySendOutcome::TransportClosed;
    }

    match tokio::time::timeout(SEND_STALL_TIMEOUT, sender.flush()).await {
        Ok(Ok(())) => AuthoritySendOutcome::Sent,
        Ok(Err(error)) => {
            error!(error = %error, "{failure_message}");
            AuthoritySendOutcome::TransportClosed
        }
        Err(_elapsed) => {
            error!(
                stall_timeout_secs = SEND_STALL_TIMEOUT.as_secs(),
                "WebSocket send stalled past the write budget (peer not draining); \\
                 treating connection as dead: {failure_message}"
            );
            AuthoritySendOutcome::TransportClosed
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
