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
        .map_err(|err| js_error(err.to_string()))
}

/// Variant of [`send_iq_command`] that surfaces RFC 6120 §8.3 stanza
/// errors as a typed [`waddle_xmpp_client::StanzaError`] instead of
/// stringifying them onto the rejected Promise. Transport / disconnect
/// failures still produce a rejected `JsValue`. Use this when the
/// caller needs to branch on the stanza error condition (e.g. treat
/// `item-not-found` on a first-publish XEP-0163 PEP fetch as an
/// empty result rather than a hard failure).
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
