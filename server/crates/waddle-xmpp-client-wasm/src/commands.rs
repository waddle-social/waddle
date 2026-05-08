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
