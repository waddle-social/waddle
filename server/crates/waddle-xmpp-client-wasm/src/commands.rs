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
        | ClientError::WebSocketWriteTimeout { .. }
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

pub(crate) async fn send_iq_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<Element, JsValue> {
    let mut cmd_tx = command_sender(&inner)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::SendIq { stanza, responder })
        .await
        .map_err(|_| js_error("client is disconnected"))?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
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

pub(crate) async fn request_stream_management_ack(
    inner: Rc<RefCell<WaddleClientInner>>,
) -> Result<(), JsValue> {
    let mut cmd_tx = command_sender(&inner)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::RequestStreamManagementAck { responder })
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
    let mut cmd_tx = command_sender(&inner)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::SendIq { stanza, responder })
        .await
        .map_err(|_| js_error("client is disconnected"))?;
    let result = rx.await.map_err(|_| js_error("client is disconnected"))?;
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
    let mut cmd_tx = command_sender(&inner).map_err(AvatarRequestFailure::Other)?;
    let (responder, rx) = oneshot::channel();
    cmd_tx
        .send(WasmCommand::SendIq { stanza, responder })
        .await
        .map_err(|_| AvatarRequestFailure::Other(js_error("client is disconnected")))?;
    rx.await
        .map_err(|_| AvatarRequestFailure::Other(js_error("client is disconnected")))?
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
    use futures::executor::block_on;
    use waddle_xmpp_client::error::{StanzaError, StanzaErrorType};

    #[test]
    fn public_disconnect_hides_sender_but_preserves_resume_until_clean_completion() {
        block_on(async {
            let (cmd_tx, mut cmd_rx) = mpsc::channel(1);
            let resume_state = waddle_xmpp_client::SmResumeState::new("public-disconnect", 2, 3)
                .expect("resume state");
            let inner = Rc::new(RefCell::new(WaddleClientInner {
                config: StoredConfig {
                    server_url: "wss://xmpp.example.test/ws".to_string(),
                    jid: "alice@example.test".to_string(),
                    access_token: "token".to_string(),
                    resource: "web".to_string(),
                    resume_state: Some(resume_state.clone()),
                },
                cmd_tx: Some(cmd_tx),
                on_message: None,
                on_presence: None,
                on_connected: None,
                on_session_lifecycle: None,
                on_stream_management: None,
                on_disconnected: None,
                on_error: None,
                on_message_delivery_acked: None,
                on_message_delivery_failed: None,
                on_mds_displayed: None,
                on_pubsub_event: None,
                on_call: None,
                resume_state: Some(resume_state.clone()),
            }));

            let acknowledge_driver = async {
                let Some(WasmCommand::Disconnect { responder }) = cmd_rx.next().await else {
                    panic!("expected disconnect command");
                };
                let _ = responder.send(Ok(()));
            };
            let (result, ()) = futures::join!(disconnect_client(inner.clone()), acknowledge_driver);

            assert!(result.is_ok());
            assert!(inner.borrow().cmd_tx.is_none());
            assert_eq!(inner.borrow().resume_state, Some(resume_state));
            assert!(disconnect_client(inner.clone()).await.is_ok());
        });
    }

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
}
