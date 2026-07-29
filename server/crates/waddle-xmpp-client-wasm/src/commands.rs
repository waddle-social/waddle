use super::*;

pub(crate) async fn send_stanza_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<(), JsValue> {
    let mut cmd_tx = command_sender(&inner)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::SendStanza { stanza, responder })
        .await
        .map_err(|_| js_error("client is disconnected"))?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

pub(crate) async fn send_message_stanza_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<(), WaddleSendMessageOutcome> {
    let mut cmd_tx = command_sender(&inner).map_err(|_| WaddleSendMessageOutcome::NotConnected)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::SendStanza { stanza, responder })
        .await
        .map_err(|_| WaddleSendMessageOutcome::NotConnected)?;
    rx.await
        .map_err(|_| WaddleSendMessageOutcome::NotConnected)?
        .map_err(|err| send_failure_outcome(&err))
}

pub(crate) fn send_failure_outcome(error: &ClientError) -> WaddleSendMessageOutcome {
    match error {
        ClientError::Disconnected => WaddleSendMessageOutcome::NotConnected,
        ClientError::StanzaError(_) => WaddleSendMessageOutcome::StanzaError,
        ClientError::InvalidTransportScheme { .. }
        | ClientError::MissingWebSocketHost
        | ClientError::WebSocketConnectTimeout { .. }
        | ClientError::TransportClosed
        | ClientError::EmptyTransportFrame
        | ClientError::TransportFrameTooLarge { .. }
        | ClientError::InvalidTransportFrame
        | ClientError::InvalidStreamOpenTo
        | ClientError::InvalidStreamOpenFrom
        | ClientError::UnsupportedStreamVersion { .. }
        | ClientError::UnsupportedWebSocketMessage => WaddleSendMessageOutcome::TransportError,
        _ => WaddleSendMessageOutcome::Error,
    }
}

/// Deadline for a client-initiated IQ round trip (#1446). RFC 6120
/// requires a reply to every IQ, but a broken or silent server can
/// simply never send one — and the reply oneshot only resolves on a
/// matched result or a disconnect sweep. Without a deadline every
/// caller up the stack (including call teardown) inherits an unbounded
/// wait.
pub(crate) const IQ_REPLY_DEADLINE_MS: u32 = 30_000;

/// Outcome of racing the driver's IQ-reply oneshot against a deadline.
pub(crate) enum IqReplyWait {
    Reply(Result<Element, ClientError>),
    /// The driver dropped the responder (disconnect sweep).
    Disconnected,
    DeadlineExpired,
}

/// Race a full IQ round trip (queue admission + reply oneshot) against
/// `deadline`. The whole trip sits inside the race on purpose: a
/// stalled driver can block the bounded command channel before the
/// stanza is even accepted, and that wait must count against the
/// deadline too. Split from the wasm entry points (which supply a real
/// `setTimeout` future) so the race itself is testable on native
/// targets.
pub(crate) async fn wait_iq_reply_with_deadline<F, D>(round_trip: F, deadline: D) -> IqReplyWait
where
    F: core::future::Future<Output = IqReplyWait>,
    D: core::future::Future<Output = ()>,
{
    use futures::future::{select, Either};
    futures::pin_mut!(round_trip);
    futures::pin_mut!(deadline);
    match select(round_trip, deadline).await {
        Either::Left((outcome, _)) => outcome,
        Either::Right(((), _)) => IqReplyWait::DeadlineExpired,
    }
}

/// Map the reply oneshot into an [`IqReplyWait`]: a dropped responder
/// is the driver's disconnect sweep.
pub(crate) async fn iq_reply_from_oneshot(
    rx: oneshot::Receiver<Result<Element, ClientError>>,
) -> IqReplyWait {
    match rx.await {
        Ok(reply) => IqReplyWait::Reply(reply),
        Err(_) => IqReplyWait::Disconnected,
    }
}

/// Resolve after `ms` on the JS event loop, plus a cancel handle that
/// clears the timer once the race is decided (so a won race doesn't
/// retain the timer + resolve closure for the rest of the deadline).
/// Reads `setTimeout`/`clearTimeout` off the global scope so it works
/// in both window and worker contexts, and compiles (unused) on native
/// test targets. If the runtime somehow lacks a timer API the promise
/// never resolves and the wait degrades to the pre-deadline behavior.
fn sleep_ms(ms: u32) -> (impl core::future::Future<Output = ()>, impl FnOnce()) {
    let timer_id = Rc::new(std::cell::Cell::new(None::<f64>));
    let timer_id_for_set = timer_id.clone();
    let promise = js_sys::Promise::new(&mut move |resolve, _reject| {
        if let Some(set_timeout) = global_timer_fn("setTimeout") {
            let scheduled = set_timeout.call2(
                &js_sys::global(),
                &resolve,
                &JsValue::from_f64(f64::from(ms)),
            );
            if let Ok(id) = scheduled {
                timer_id_for_set.set(id.as_f64());
            }
        }
    });
    let cancel = move || {
        if let (Some(id), Some(clear_timeout)) = (timer_id.get(), global_timer_fn("clearTimeout")) {
            let _ = clear_timeout.call1(&js_sys::global(), &JsValue::from_f64(id));
        }
    };
    let future = async move {
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    };
    (future, cancel)
}

fn global_timer_fn(name: &str) -> Option<js_sys::Function> {
    use wasm_bindgen::JsCast;
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.dyn_into::<js_sys::Function>().ok())
}

/// Send an IQ and await its reply under [`IQ_REPLY_DEADLINE_MS`].
/// Shared by [`send_iq_command`], [`send_iq_command_stanza_aware`],
/// and [`send_avatar_iq_command`].
async fn send_iq_roundtrip(
    inner: &Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<Result<Element, ClientError>, JsValue> {
    let mut cmd_tx = command_sender(inner)?;
    let cancel_tx = cmd_tx.clone();
    // Parsed once into the typed correlation id; an absent/empty id
    // simply means there is nothing to cancel on expiry.
    let iq_id = stanza
        .attr("id")
        .and_then(|value| waddle_xmpp_client::request::StanzaId::new(value).ok());
    let (responder, rx) = oneshot::channel();
    let round_trip = async move {
        if cmd_tx
            .send(WasmCommand::SendIq { stanza, responder })
            .await
            .is_err()
        {
            return IqReplyWait::Disconnected;
        }
        iq_reply_from_oneshot(rx).await
    };
    let (deadline, cancel_deadline) = sleep_ms(IQ_REPLY_DEADLINE_MS);
    let outcome = wait_iq_reply_with_deadline(round_trip, deadline).await;
    cancel_deadline();
    match outcome {
        IqReplyWait::Reply(reply) => Ok(reply),
        IqReplyWait::Disconnected => Err(js_error("client is disconnected")),
        IqReplyWait::DeadlineExpired => {
            // Free the driver's pending-IQ slot (and any deferred copy of
            // the command) so a reply that limps in later has nowhere to
            // land. Spawned so a momentarily full command queue delays
            // the cancellation instead of dropping it — the expired
            // caller must not block on it either way.
            if let Some(id) = iq_id {
                let mut cancel_tx = cancel_tx;
                wasm_bindgen_futures::spawn_local(async move {
                    let (cancel_responder, _cancel_rx) = oneshot::channel();
                    let _ = cancel_tx
                        .send(WasmCommand::CancelIq {
                            id,
                            responder: cancel_responder,
                        })
                        .await;
                });
            }
            // Typed until the WASM boundary: callers map this like any
            // other ClientError (send_iq_command stringifies it in
            // iq_rejection; the stanza-aware/avatar variants at their
            // own boundary arms).
            Ok(Err(ClientError::IqTimeout {
                timeout: std::time::Duration::from_millis(u64::from(IQ_REPLY_DEADLINE_MS)),
            }))
        }
    }
}

pub(crate) async fn send_iq_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<Element, JsValue> {
    send_iq_roundtrip(&inner, stanza)
        .await?
        .map_err(iq_rejection)
}

/// Fields the JS side reads off a rejected IQ promise for an RFC 6120
/// §8.3 stanza error (`stanzaErrorContext` in chat's `client-muc-admin.ts`).
/// Split from [`iq_rejection`] so the mapping is testable on native targets,
/// where `JsValue` construction cannot run.
pub(crate) struct StanzaErrorRejectionFields {
    pub message: String,
    pub condition: String,
    pub error_type: &'static str,
    pub text: Option<String>,
}

pub(crate) fn stanza_error_rejection_fields(
    err: &ClientError,
) -> Option<StanzaErrorRejectionFields> {
    let ClientError::StanzaError(stanza_err) = err else {
        return None;
    };
    Some(StanzaErrorRejectionFields {
        message: err.to_string(),
        condition: stanza_err.condition.clone(),
        error_type: stanza_err.error_type.as_str(),
        text: stanza_err.text.clone(),
    })
}

/// Reject an IQ promise. Stanza errors become a real JS `Error` (so
/// `String(err)` / console paths keep a readable message) carrying
/// `condition`, `errorType`, and `text` properties for telemetry;
/// everything else stays a stringified rejection.
pub(crate) fn iq_rejection(err: ClientError) -> JsValue {
    let Some(fields) = stanza_error_rejection_fields(&err) else {
        return js_error(err.to_string());
    };
    let js_err = js_sys::Error::new(&fields.message);
    set_rejection_property(&js_err, "condition", &fields.condition);
    set_rejection_property(&js_err, "errorType", fields.error_type);
    if let Some(text) = &fields.text {
        set_rejection_property(&js_err, "text", text);
    }
    js_err.into()
}

fn set_rejection_property(target: &js_sys::Error, key: &str, value: &str) {
    // Reflect::set only fails on non-objects; an Error is always an
    // object, so a failure would leave the property absent and the JS
    // side falls back to its condition-less path.
    let _ = js_sys::Reflect::set(
        target.as_ref(),
        &JsValue::from_str(key),
        &JsValue::from_str(value),
    );
}

pub(crate) async fn cancel_iq_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    id: String,
) -> Result<(), JsValue> {
    let id =
        waddle_xmpp_client::request::StanzaId::new(id).map_err(|err| js_error(err.to_string()))?;
    let mut cmd_tx = command_sender(&inner)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::CancelIq { id, responder })
        .await
        .map_err(|_| js_error("client is disconnected"))?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

/// Variant of [`send_iq_command`] that surfaces RFC 6120 §8.3 stanza
/// errors as a typed [`waddle_xmpp_client::StanzaError`] on the Rust
/// side instead of rejecting the Promise. Transport / disconnect
/// failures still produce a rejected `JsValue`. Use this when Rust
/// code needs to branch on the stanza error condition (e.g. treat
/// `item-not-found` on a first-publish XEP-0163 PEP fetch as an
/// empty result rather than a hard failure); JS-side consumers can
/// instead read the `condition` property off [`send_iq_command`]'s
/// rejection.
pub(crate) async fn send_iq_command_stanza_aware(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<Result<Element, waddle_xmpp_client::StanzaError>, JsValue> {
    let result = send_iq_roundtrip(&inner, stanza).await?;
    match result {
        Ok(elem) => Ok(Ok(elem)),
        Err(ClientError::StanzaError(stanza_err)) => Ok(Err(stanza_err)),
        Err(other) => Err(js_error(other.to_string())),
    }
}

pub(crate) async fn send_avatar_iq_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<Element, AvatarRequestFailure<JsValue>> {
    send_iq_roundtrip(&inner, stanza)
        .await
        .map_err(AvatarRequestFailure::Other)?
        .map_err(|err| match err {
            ClientError::StanzaError(_) => AvatarRequestFailure::StanzaError,
            other => AvatarRequestFailure::Other(js_error(other.to_string())),
        })
}

pub(crate) async fn send_mam_query_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
    query_id: String,
) -> Result<waddle_xmpp_client::MamPage, JsValue> {
    let mut cmd_tx = command_sender(&inner)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::SendMamQuery {
            stanza,
            query_id,
            responder,
        })
        .await
        .map_err(|_| js_error("client is disconnected"))?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

pub(crate) async fn send_inbox_query_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
    query_id: String,
) -> Result<crate::state::InboxPage, JsValue> {
    let mut cmd_tx = command_sender(&inner)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::SendInboxQuery {
            stanza,
            query_id,
            responder,
        })
        .await
        .map_err(|_| js_error("client is disconnected"))?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

pub(crate) async fn disconnect_client(
    inner: Rc<RefCell<WaddleClientInner>>,
) -> Result<(), JsValue> {
    let mut cmd_tx = match inner.borrow().cmd_tx.clone() {
        Some(cmd_tx) => cmd_tx,
        None => return Ok(()),
    };

    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::Disconnect { responder })
        .await
        .map_err(|_| js_error("client is disconnected"))?;
    inner.borrow_mut().cmd_tx = None;
    inner.borrow_mut().resume_state = None;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

pub(crate) fn command_sender(
    inner: &Rc<RefCell<WaddleClientInner>>,
) -> Result<mpsc::Sender<WasmCommand>, JsValue> {
    inner
        .borrow()
        .cmd_tx
        .clone()
        .ok_or_else(|| js_error("client is not connected"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp_client::error::{StanzaError, StanzaErrorType};

    fn stanza_error(
        error_type: StanzaErrorType,
        condition: &str,
        text: Option<&str>,
    ) -> ClientError {
        ClientError::StanzaError(StanzaError {
            error_type,
            condition: condition.to_string(),
            text: text.map(str::to_string),
            application_condition: None,
        })
    }

    #[test]
    fn stanza_error_rejection_fields_carry_condition_type_and_text() {
        let err = stanza_error(
            StanzaErrorType::Cancel,
            "item-not-found",
            Some("no such room"),
        );
        let fields = stanza_error_rejection_fields(&err).expect("stanza errors yield fields");
        assert_eq!(fields.condition, "item-not-found");
        assert_eq!(fields.error_type, "cancel");
        assert_eq!(fields.text.as_deref(), Some("no such room"));
        assert_eq!(fields.message, err.to_string());
    }

    #[test]
    fn stanza_error_rejection_fields_omit_absent_text() {
        let err = stanza_error(StanzaErrorType::Auth, "forbidden", None);
        let fields = stanza_error_rejection_fields(&err).expect("stanza errors yield fields");
        assert_eq!(fields.condition, "forbidden");
        assert_eq!(fields.error_type, "auth");
        assert_eq!(fields.text, None);
    }

    #[test]
    fn unrecognised_error_type_labels_as_unknown() {
        let err = stanza_error(StanzaErrorType::Unknown, "service-unavailable", None);
        let fields = stanza_error_rejection_fields(&err).expect("stanza errors yield fields");
        assert_eq!(fields.error_type, "unknown");
    }

    #[test]
    fn non_stanza_errors_yield_no_fields() {
        assert!(stanza_error_rejection_fields(&ClientError::Disconnected).is_none());
    }

    fn iq_result_element() -> Element {
        Element::builder("iq", "jabber:client")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .build()
    }

    #[test]
    fn iq_reply_wait_delivers_reply_when_it_beats_the_deadline() {
        let (tx, rx) = oneshot::channel();
        tx.send(Ok(iq_result_element()))
            .unwrap_or_else(|_| panic!("receiver alive"));
        let outcome = futures::executor::block_on(wait_iq_reply_with_deadline(
            iq_reply_from_oneshot(rx),
            futures::future::pending::<()>(),
        ));
        match outcome {
            IqReplyWait::Reply(Ok(elem)) => assert_eq!(elem.name(), "iq"),
            _ => panic!("reply must win over a pending deadline"),
        }
    }

    #[test]
    fn iq_reply_wait_reports_disconnect_when_responder_is_dropped() {
        let (tx, rx) = oneshot::channel::<Result<Element, ClientError>>();
        drop(tx);
        let outcome = futures::executor::block_on(wait_iq_reply_with_deadline(
            iq_reply_from_oneshot(rx),
            futures::future::pending::<()>(),
        ));
        assert!(matches!(outcome, IqReplyWait::Disconnected));
    }

    #[test]
    fn iq_reply_wait_expires_instead_of_waiting_forever() {
        // #1446: the reply oneshot must never be awaited bare — a server
        // that goes silent after the send would otherwise hang the JS
        // promise (and anything awaiting it, like call teardown) forever.
        let (_tx, rx) = oneshot::channel::<Result<Element, ClientError>>();
        let outcome = futures::executor::block_on(wait_iq_reply_with_deadline(
            iq_reply_from_oneshot(rx),
            futures::future::ready(()),
        ));
        assert!(matches!(outcome, IqReplyWait::DeadlineExpired));
    }

    #[test]
    fn iq_reply_wait_expires_even_when_queue_admission_stalls() {
        // The deadline must cover the bounded command-channel send too:
        // a stalled driver that never accepts the command is the same
        // user-visible hang as a server that never replies.
        let outcome = futures::executor::block_on(wait_iq_reply_with_deadline(
            futures::future::pending::<IqReplyWait>(),
            futures::future::ready(()),
        ));
        assert!(matches!(outcome, IqReplyWait::DeadlineExpired));
    }
}
