use super::*;

pub(crate) async fn send_stanza_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<(), JsValue> {
    let (responder, rx) = oneshot::channel();
    enqueue_command(&inner, WasmCommand::SendStanza { stanza, responder }).await?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

pub(crate) async fn send_message_stanza_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<(), WaddleSendMessageOutcome> {
    let (responder, rx) = oneshot::channel();
    enqueue_command(&inner, WasmCommand::SendStanza { stanza, responder })
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

pub(crate) async fn send_iq_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
) -> Result<Element, JsValue> {
    let (responder, rx) = oneshot::channel();
    enqueue_command(&inner, WasmCommand::SendIq { stanza, responder }).await?;
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
    let (responder, rx) = oneshot::channel();
    enqueue_command(&inner, WasmCommand::CancelIq { id, responder }).await?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

pub(crate) async fn request_stream_management_ack_command(
    inner: Rc<RefCell<WaddleClientInner>>,
) -> Result<(), JsValue> {
    let (responder, rx) = oneshot::channel();
    enqueue_command(
        &inner,
        WasmCommand::RequestStreamManagementAck { responder },
    )
    .await?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

/// Closed synchronous pagehide result exposed to the browser binding. This is
/// deliberately a wasm enum rather than a stringly-typed transport status.
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagehideSmAckEnqueueOutcome {
    Sent,
    AlreadyPending,
    Full,
    Closed,
    Busy,
    WriteFailed,
}

/// Write a typed XEP-0198 `<r/>` through the active driver's exact physical
/// socket before a browser pagehide handler returns. The runtime state is
/// committed only after `WebSocket.send` succeeds.
pub(crate) fn try_request_stream_management_ack_for_pagehide(
    inner: &Rc<RefCell<WaddleClientInner>>,
) -> PagehideSmAckEnqueueOutcome {
    let Ok(inner_ref) = inner.try_borrow() else {
        return PagehideSmAckEnqueueOutcome::Busy;
    };
    let Some(core) = inner_ref.driver_core.clone() else {
        return PagehideSmAckEnqueueOutcome::Closed;
    };
    let Some(command_lane) = inner_ref.command_lane.clone() else {
        return PagehideSmAckEnqueueOutcome::Closed;
    };
    drop(inner_ref);

    let Ok(mut core) = core.try_borrow_mut() else {
        return PagehideSmAckEnqueueOutcome::Busy;
    };
    let Ok(mut command_lane) = command_lane.try_borrow_mut() else {
        return PagehideSmAckEnqueueOutcome::Busy;
    };
    if command_lane.is_closed() {
        return PagehideSmAckEnqueueOutcome::Closed;
    }

    let mut completions = Vec::new();
    while let Some(command) = command_lane.pop_ready() {
        match drain_pagehide_command(inner, &mut core, command, &mut completions) {
            Ok(()) => {}
            Err(()) => {
                // A failed physical write closes the same admission lane the
                // async driver observes. Remaining accepted commands lose
                // their responders as a normal disconnect, never execute
                // after the `<r/>`, and cannot be overtaken on a later turn.
                drop(command_lane.close());
                for completion in completions {
                    command_lane.push_pagehide_completion(completion);
                }
                return PagehideSmAckEnqueueOutcome::WriteFailed;
            }
        }
    }

    if core.runtime.prepare_pagehide_ack() == PagehideAckRequest::AlreadyPending {
        for completion in completions {
            command_lane.push_pagehide_completion(completion);
        }
        return PagehideSmAckEnqueueOutcome::AlreadyPending;
    }

    let message = TransportMessage::Element(SmState::build_request_ack());
    let Ok(frame) = waddle_xmpp_client::encode_message(&message) else {
        for completion in completions {
            command_lane.push_pagehide_completion(completion);
        }
        return PagehideSmAckEnqueueOutcome::WriteFailed;
    };
    if core.web_socket.send_with_str(&frame).is_err() {
        for completion in completions {
            command_lane.push_pagehide_completion(completion);
        }
        return PagehideSmAckEnqueueOutcome::WriteFailed;
    }

    let outcome = if core.runtime.commit_pagehide_ack_written(chrono::Utc::now()) {
        publish_resume_state_snapshot(inner, &core.runtime, false);
        PagehideSmAckEnqueueOutcome::Sent
    } else {
        PagehideSmAckEnqueueOutcome::AlreadyPending
    };
    for completion in completions {
        command_lane.push_pagehide_completion(completion);
    }
    outcome
}

/// Synchronous subset of the regular driver used only while the browser is
/// executing `pagehide`. It performs the physical write and authoritative
/// runtime transition now, then leaves Promise completion/query bookkeeping
/// for the normal driver wakeup. No core borrow survives an await.
fn drain_pagehide_command(
    inner: &Rc<RefCell<WaddleClientInner>>,
    core: &mut WasmDriverCore,
    command: WasmCommand,
    completions: &mut Vec<PagehideCommandCompletion>,
) -> Result<(), ()> {
    match command {
        WasmCommand::SendStanza { stanza, responder } => {
            if !core.runtime.can_send_app_stanza() {
                completions.push(PagehideCommandCompletion::Deferred(
                    DeferredWasmCommand::Stanza { stanza, responder },
                ));
                return Ok(());
            }
            let result = pagehide_send_transport_message(
                inner,
                core,
                TransportMessage::Element(stanza),
                completions,
            );
            let failed = result.is_err();
            completions.push(PagehideCommandCompletion::Stanza { responder, result });
            if failed {
                Err(())
            } else {
                Ok(())
            }
        }
        WasmCommand::SendIq { stanza, responder } => {
            if !core.runtime.can_send_app_stanza() {
                completions.push(PagehideCommandCompletion::Deferred(
                    DeferredWasmCommand::Iq { stanza, responder },
                ));
                return Ok(());
            }
            let result = pagehide_send_transport_message(
                inner,
                core,
                TransportMessage::Element(stanza.clone()),
                completions,
            );
            let failed = result.is_err();
            completions.push(PagehideCommandCompletion::Iq {
                stanza,
                responder,
                result,
            });
            if failed {
                Err(())
            } else {
                Ok(())
            }
        }
        WasmCommand::SendMamQuery {
            stanza,
            query_id,
            responder,
        } => {
            if !core.runtime.can_send_app_stanza() {
                completions.push(PagehideCommandCompletion::Deferred(
                    DeferredWasmCommand::MamQuery {
                        stanza,
                        query_id,
                        responder,
                    },
                ));
                return Ok(());
            }
            let result = pagehide_send_transport_message(
                inner,
                core,
                TransportMessage::Element(stanza.clone()),
                completions,
            );
            let failed = result.is_err();
            completions.push(PagehideCommandCompletion::MamQuery {
                stanza,
                query_id,
                responder,
                result,
            });
            if failed {
                Err(())
            } else {
                Ok(())
            }
        }
        WasmCommand::SendInboxQuery {
            stanza,
            query_id,
            responder,
        } => {
            if !core.runtime.can_send_app_stanza() {
                completions.push(PagehideCommandCompletion::Deferred(
                    DeferredWasmCommand::InboxQuery {
                        stanza,
                        query_id,
                        responder,
                    },
                ));
                return Ok(());
            }
            let result = pagehide_send_transport_message(
                inner,
                core,
                TransportMessage::Element(stanza.clone()),
                completions,
            );
            let failed = result.is_err();
            completions.push(PagehideCommandCompletion::InboxQuery {
                stanza,
                query_id,
                responder,
                result,
            });
            if failed {
                Err(())
            } else {
                Ok(())
            }
        }
        WasmCommand::CancelIq { id, responder } => {
            completions.push(PagehideCommandCompletion::CancelIq { id, responder });
            Ok(())
        }
        WasmCommand::Disconnect { responder } => {
            let result = pagehide_send_transport_message(
                inner,
                core,
                TransportMessage::Close(StreamClose),
                completions,
            );
            let failed = result.is_err();
            completions.push(PagehideCommandCompletion::Disconnect { responder, result });
            if failed {
                Err(())
            } else {
                Ok(())
            }
        }
        WasmCommand::RequestStreamManagementAck { responder } => {
            let mut result = Ok(());
            for event in core.runtime.request_stream_management_ack() {
                if let ClientEvent::Connection(ConnectionEvent::OutboundMessage(message)) = event {
                    if let Err(err) =
                        pagehide_send_transport_message(inner, core, message, completions)
                    {
                        result = Err(err);
                        break;
                    }
                } else {
                    completions.push(PagehideCommandCompletion::Event(event));
                }
            }
            let failed = result.is_err();
            completions.push(PagehideCommandCompletion::StreamManagementAck { responder, result });
            if failed {
                Err(())
            } else {
                Ok(())
            }
        }
    }
}

fn pagehide_send_transport_message(
    inner: &Rc<RefCell<WaddleClientInner>>,
    core: &mut WasmDriverCore,
    message: TransportMessage,
    completions: &mut Vec<PagehideCommandCompletion>,
) -> DriverResult<()> {
    let frame = waddle_xmpp_client::encode_message(&message)?;
    core.web_socket
        .send_with_str(&frame)
        .map_err(|_| ClientError::TransportClosed)?;
    if matches!(message, TransportMessage::Close(_)) {
        let _ = core.web_socket.close();
    }
    for event in apply_pagehide_message_sent(inner, &mut core.runtime, message)? {
        if let ClientEvent::Connection(ConnectionEvent::OutboundMessage(message)) = event {
            pagehide_send_transport_message(inner, core, message, completions)?;
        } else {
            completions.push(PagehideCommandCompletion::Event(event));
        }
    }
    Ok(())
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
    let (responder, rx) = oneshot::channel();
    enqueue_command(&inner, WasmCommand::SendIq { stanza, responder }).await?;
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
    let (responder, rx) = oneshot::channel();
    enqueue_command(&inner, WasmCommand::SendIq { stanza, responder })
        .await
        .map_err(AvatarRequestFailure::Other)?;
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
    let (responder, rx) = oneshot::channel();
    enqueue_command(
        &inner,
        WasmCommand::SendMamQuery {
            stanza,
            query_id,
            responder,
        },
    )
    .await?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

pub(crate) async fn send_inbox_query_command(
    inner: Rc<RefCell<WaddleClientInner>>,
    stanza: Element,
    query_id: String,
) -> Result<crate::state::InboxPage, JsValue> {
    let (responder, rx) = oneshot::channel();
    enqueue_command(
        &inner,
        WasmCommand::SendInboxQuery {
            stanza,
            query_id,
            responder,
        },
    )
    .await?;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

pub(crate) async fn disconnect_client(
    inner: Rc<RefCell<WaddleClientInner>>,
) -> Result<(), JsValue> {
    let connected = match inner.borrow().command_lane.clone() {
        Some(lane) if !lane.borrow().is_closed() => true,
        None => return Ok(()),
        Some(_) => return Ok(()),
    };
    debug_assert!(connected);

    let (responder, rx) = oneshot::channel();
    enqueue_command(&inner, WasmCommand::Disconnect { responder }).await?;
    inner.borrow_mut().command_lane = None;
    inner.borrow_mut().resume_state = None;
    rx.await
        .map_err(|_| js_error("client is disconnected"))?
        .map_err(|err| js_error(err.to_string()))
}

/// Enqueue a browser command while preserving the former async-channel
/// backpressure. Commands are only considered admitted once they occupy the
/// shared ready FIFO; pagehide drains that same FIFO synchronously.
pub(crate) async fn enqueue_command(
    inner: &Rc<RefCell<WaddleClientInner>>,
    command: WasmCommand,
) -> Result<(), JsValue> {
    let lane = inner
        .borrow()
        .command_lane
        .clone()
        .ok_or_else(|| js_error("client is not connected"))?;
    let admission = lane
        .borrow_mut()
        .enqueue(command)
        .map_err(|_| js_error("client is disconnected"))?;
    if let Some(admission) = admission {
        admission
            .await
            .map_err(|_| js_error("client is disconnected"))?;
    }
    Ok(())
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
}
