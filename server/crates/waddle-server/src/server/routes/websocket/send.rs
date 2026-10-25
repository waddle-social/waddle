use super::{debug, error, Arc, Message, Sink, SinkExt};
use crate::clustering::NodeAdmissionPermit;

/// Interrupt only socket I/O when a verified resume targets this exact live
/// connection. Protocol handlers finish their current durable work normally.
pub(super) struct InterruptibleSink<S> {
    inner: S,
    interrupt: tokio_util::sync::CancellationToken,
    cancelled: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    resume_control: Option<Arc<waddle_xmpp::registry::LiveResumeControl>>,
}

impl<S> InterruptibleSink<S> {
    pub(super) fn new(inner: S, interrupt: tokio_util::sync::CancellationToken) -> Self {
        let cancelled = Box::pin(interrupt.clone().cancelled_owned());
        Self {
            inner,
            interrupt,
            cancelled,
            resume_control: None,
        }
    }

    pub(super) fn with_resume_control(
        mut self,
        control: Arc<waddle_xmpp::registry::LiveResumeControl>,
    ) -> Self {
        self.resume_control = Some(control);
        self
    }

    fn poll_resume_control(&self, cx: &mut std::task::Context<'_>) {
        if let Some(control) = &self.resume_control {
            let _ = control.poll(cx);
        }
    }

    /// Conflict closure is optional and bounded by its caller, but still gets
    /// one attempt after ordinary writes have been interrupted.
    pub(super) fn uninterrupted(&mut self) -> &mut S {
        &mut self.inner
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum InterruptedWrite<E> {
    #[error("connection force-detached")]
    Detached,
    #[error("{0}")]
    Transport(E),
}

/// Classify expected resume interruption without hiding transport failures.
pub(super) trait WebSocketWriteError: std::fmt::Display {
    fn is_detached(&self) -> bool {
        false
    }
}

impl WebSocketWriteError for axum::Error {}

impl WebSocketWriteError for std::io::Error {}

#[cfg(test)]
impl WebSocketWriteError for &'static str {}

#[cfg(test)]
impl WebSocketWriteError for std::convert::Infallible {}

impl<E: std::fmt::Display> WebSocketWriteError for InterruptedWrite<E> {
    fn is_detached(&self) -> bool {
        matches!(self, Self::Detached)
    }
}

fn log_write_failure(error: &impl WebSocketWriteError, failure_message: &'static str) {
    if error.is_detached() {
        debug!(error = %error, "{failure_message}");
    } else {
        error!(error = %error, "{failure_message}");
    }
}

impl<S: Sink<Message> + Unpin> Sink<Message> for InterruptibleSink<S> {
    type Error = InterruptedWrite<S::Error>;

    fn poll_ready(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        this.poll_resume_control(cx);
        if std::future::Future::poll(this.cancelled.as_mut(), cx).is_ready() {
            return std::task::Poll::Ready(Err(InterruptedWrite::Detached));
        }
        std::pin::Pin::new(&mut this.inner)
            .poll_ready(cx)
            .map_err(InterruptedWrite::Transport)
    }

    fn start_send(self: std::pin::Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        let this = self.get_mut();
        if this.interrupt.is_cancelled() {
            return Err(InterruptedWrite::Detached);
        }
        std::pin::Pin::new(&mut this.inner)
            .start_send(item)
            .map_err(InterruptedWrite::Transport)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        this.poll_resume_control(cx);
        if std::future::Future::poll(this.cancelled.as_mut(), cx).is_ready() {
            return std::task::Poll::Ready(Err(InterruptedWrite::Detached));
        }
        std::pin::Pin::new(&mut this.inner)
            .poll_flush(cx)
            .map_err(InterruptedWrite::Transport)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        this.poll_resume_control(cx);
        if std::future::Future::poll(this.cancelled.as_mut(), cx).is_ready() {
            return std::task::Poll::Ready(Err(InterruptedWrite::Detached));
        }
        std::pin::Pin::new(&mut this.inner)
            .poll_close(cx)
            .map_err(InterruptedWrite::Transport)
    }
}

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
    E: WebSocketWriteError,
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
    authority: (&NodeAdmissionPermit, &tokio_util::sync::CancellationToken),
) -> AuthoritySendOutcome
where
    S: Sink<Message, Error = E> + Unpin,
    E: WebSocketWriteError,
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
    E: WebSocketWriteError,
{
    match tokio::time::timeout(SEND_STALL_TIMEOUT, sender.send(message)).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            log_write_failure(&error, failure_message);
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
    authority: Option<(&NodeAdmissionPermit, &tokio_util::sync::CancellationToken)>,
) -> AuthoritySendOutcome
where
    S: Sink<Message, Error = E> + Unpin,
    E: WebSocketWriteError,
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
            log_write_failure(&error, failure_message);
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
        log_write_failure(&error, failure_message);
        return AuthoritySendOutcome::TransportClosed;
    }

    match tokio::time::timeout(SEND_STALL_TIMEOUT, sender.flush()).await {
        Ok(Ok(())) => AuthoritySendOutcome::Sent,
        Ok(Err(error)) => {
            log_write_failure(&error, failure_message);
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
    E: WebSocketWriteError,
{
    match tokio::time::timeout(SEND_STALL_TIMEOUT, sender.close()).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            log_write_failure(&error, failure_message);
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
