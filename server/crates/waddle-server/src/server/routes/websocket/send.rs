use super::connection::{RetirementFence, RetirementRefusal};
use super::{debug, error, Arc, Message, Sink, SinkExt};
use crate::clustering::NodeAdmissionPermit;
use waddle_xmpp::ownership::ConnectionRef;

/// Interrupt ordinary writes when a verified detach targets this physical
/// connection, and refuse them once the connection has retired. Protocol
/// handlers finish their current durable work normally; bounded conflict and
/// close writes may explicitly bypass this wrapper.
pub(super) struct InterruptibleSink<S> {
    inner: S,
    interrupt: waddle_xmpp::registry::SocketWriteInterruption,
    cancelled: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    resume_control: Option<Arc<waddle_xmpp::registry::LiveResumeControl>>,
    // The fence, and the generation every write of this sink belongs to.
    retirement: Option<(Arc<RetirementFence>, ConnectionRef)>,
    // Set only while a `RetirementClose` borrows this sink.
    retirement_close: bool,
}

impl<S> InterruptibleSink<S> {
    pub(super) fn new(inner: S, interrupt: waddle_xmpp::registry::SocketWriteInterruption) -> Self {
        let cancelled = Box::pin(interrupt.clone().cancelled_owned());
        Self {
            inner,
            interrupt,
            cancelled,
            resume_control: None,
            retirement: None,
            retirement_close: false,
        }
    }

    pub(super) fn with_resume_control(
        mut self,
        control: Arc<waddle_xmpp::registry::LiveResumeControl>,
    ) -> Self {
        self.resume_control = Some(control);
        self
    }

    /// Refuse ordinary writes once `fence` has retired `generation`, the
    /// physical connection this sink writes to.
    pub(super) fn with_retirement(
        mut self,
        fence: Arc<RetirementFence>,
        generation: ConnectionRef,
    ) -> Self {
        self.retirement = Some((fence, generation));
        self
    }

    fn poll_resume_control(&self, cx: &mut std::task::Context<'_>) {
        if let Some(control) = &self.resume_control {
            let _ = control.poll(cx);
        }
    }

    /// Why the retirement fence refuses this write, unless the write is part
    /// of the retirement's own close.
    fn retired(&self) -> Option<RetirementRefusal> {
        if self.retirement_close {
            return None;
        }
        self.retirement
            .as_ref()
            .and_then(|(fence, generation)| fence.admit(generation).err())
    }

    /// Conflict closure is optional and bounded by its caller, but still gets
    /// one attempt after ordinary writes have been interrupted.
    pub(super) fn uninterrupted(&mut self) -> &mut S {
        &mut self.inner
    }

    /// The sink a retirement writes its own close through. It stays subject
    /// to write interruption; only the retirement fence is lifted, and only
    /// while the returned borrow lives.
    pub(super) fn retirement_close(&mut self) -> RetirementClose<'_, S> {
        self.retirement_close = true;
        RetirementClose(self)
    }
}

/// See [`InterruptibleSink::retirement_close`].
pub(super) struct RetirementClose<'a, S>(&'a mut InterruptibleSink<S>);

impl<S> Drop for RetirementClose<'_, S> {
    fn drop(&mut self) {
        self.0.retirement_close = false;
    }
}

impl<S: Sink<Message> + Unpin> Sink<Message> for RetirementClose<'_, S> {
    type Error = InterruptedWrite<S::Error>;

    fn poll_ready(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut *self.get_mut().0).poll_ready(cx)
    }

    fn start_send(self: std::pin::Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        std::pin::Pin::new(&mut *self.get_mut().0).start_send(item)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut *self.get_mut().0).poll_flush(cx)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut *self.get_mut().0).poll_close(cx)
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum InterruptedWrite<E> {
    #[error("connection force-detached")]
    Detached,
    /// The retirement fence refused this write before it started. Nothing
    /// was enqueued.
    #[error("{0}")]
    Retired(RetirementRefusal),
    #[error("{0}")]
    Transport(E),
}

/// Classify a write the connection itself interrupted, for a resume or for
/// its own retirement, without hiding transport failures.
pub(super) trait WebSocketWriteError: std::fmt::Display {
    fn is_detached(&self) -> bool {
        false
    }
}

impl WebSocketWriteError for axum::Error {}

impl WebSocketWriteError for std::io::Error {}

impl WebSocketWriteError for crate::websocket_transport::OutputError<axum::Error> {}

#[cfg(test)]
impl WebSocketWriteError for &'static str {}

#[cfg(test)]
impl WebSocketWriteError for std::convert::Infallible {}

impl<E: std::fmt::Display> WebSocketWriteError for InterruptedWrite<E> {
    fn is_detached(&self) -> bool {
        matches!(self, Self::Detached | Self::Retired(_))
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
        if let Some(refusal) = this.retired() {
            return Err(InterruptedWrite::Retired(refusal));
        }
        this.interrupt
            .with_uninterrupted(|| std::pin::Pin::new(&mut this.inner).start_send(item))
            .ok_or(InterruptedWrite::Detached)?
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
/// `start_send` is the transport commit point for a sink that enqueues
/// straight into the socket: once it has run under a valid permit, a later
/// lifecycle transition cannot retract the frame. Before that point,
/// revocation must suppress the write entirely. A pump-backed sink applies
/// the same fence again at the pump's own enqueue; a frame the pump did not
/// finish because the authority was revoked is reported as revoked.
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
/// cancellable, then validate the bound source revision and generation under
/// read guards held only for the synchronous `start_send`. In the `SplitSink` path this is a local
/// enqueue: after it returns, a later flush may still reach the WebSocket
/// after revocation as already-authorized in-flight work. The independent
/// shutdown token check remains best effort and is not atomic with that
/// enqueue.
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
    send_ws_message_with_binding(sender, message, failure_message, authority, None).await
}

/// Node admission, socket control, binding cell, and physical interruption
/// gate are held in that order only for synchronous `start_send`.
pub(super) async fn send_ws_message_with_binding<S, E>(
    sender: &mut S,
    message: Message,
    failure_message: &'static str,
    authority: Option<(&NodeAdmissionPermit, &tokio_util::sync::CancellationToken)>,
    binding: Option<(
        &waddle_xmpp::registry::LocalBindingPermit,
        &waddle_xmpp::registry::LiveResumeControl,
    )>,
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
    if binding.is_some_and(|(permit, _)| !permit.is_active()) {
        return AuthoritySendOutcome::TransportClosed;
    }

    // `futures::SinkExt` does not expose a portable `ready` future for every
    // Sink version we support. Poll the required `Sink::poll_ready` directly
    // so cancellation remains selectable while transport readiness is parked.
    let ready_result = {
        let ready = std::future::poll_fn(|cx| std::pin::Pin::new(&mut *sender).poll_ready(cx));
        tokio::pin!(ready);
        tokio::select! {
            biased;
            _ = async {
                match authority {
                    Some((permit, shutdown)) => tokio::select! {
                        _ = shutdown.cancelled() => {},
                        _ = permit.revoked() => {},
                    },
                    None => std::future::pending().await,
                }
            } => return AuthoritySendOutcome::AuthorityRevoked,
            _ = async {
                match binding {
                    Some((permit, _)) => permit.revoked().await,
                    None => std::future::pending().await,
                }
            } => return AuthoritySendOutcome::TransportClosed,
            result = tokio::time::timeout(SEND_STALL_TIMEOUT, ready.as_mut()) => result,
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
    // returned Ready in the same poll that wakes this task. Check the
    // independent shutdown token again, then hold the bound source and
    // lifecycle guards only across synchronous `start_send` so neither can
    // change between validation and local enqueue.
    enum StartError<E> {
        BindingRevoked,
        Transport(E),
    }
    let start = || {
        let result = match binding {
            Some((permit, control)) => control.start_sm_enable_if_current(permit, || {
                std::pin::Pin::new(&mut *sender).start_send(message)
            }),
            None => Some(std::pin::Pin::new(&mut *sender).start_send(message)),
        };
        result
            .ok_or(StartError::BindingRevoked)?
            .map_err(StartError::Transport)
    };
    let start_send_result = match authority {
        Some((permit, shutdown)) => {
            if shutdown.is_cancelled() {
                return AuthoritySendOutcome::AuthorityRevoked;
            }
            permit.start_send_if_current(start)
        }
        None => Ok(start()),
    };
    match start_send_result {
        Err(_revoked) => return AuthoritySendOutcome::AuthorityRevoked,
        Ok(Err(StartError::BindingRevoked)) => return AuthoritySendOutcome::TransportClosed,
        Ok(Err(StartError::Transport(error))) => {
            log_write_failure(&error, failure_message);
            return AuthoritySendOutcome::TransportClosed;
        }
        Ok(Ok(())) => {}
    }

    // The admission guard has been released. A revoke may now complete while
    // this already-authorized local enqueue is being flushed.
    match tokio::time::timeout(SEND_STALL_TIMEOUT, sender.flush()).await {
        Ok(Ok(())) => AuthoritySendOutcome::Sent,
        // A pump-backed sink reports a frame it refused or abandoned at its
        // own authority fence as a flush error. That is the revocation, not a
        // broken transport, and the caller's revocation path still applies.
        Ok(Err(error)) if !authoritative() => {
            debug!(error = %error, "{failure_message}: authority was revoked before the write finished");
            AuthoritySendOutcome::AuthorityRevoked
        }
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
