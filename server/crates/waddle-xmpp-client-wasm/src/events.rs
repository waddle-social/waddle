use super::*;

pub(crate) async fn event_dispatch_loop(
    inner: Rc<RefCell<WaddleClientInner>>,
    mut event_rx: mpsc::Receiver<DriverEvent>,
) {
    while let Some(event) = event_rx.next().await {
        match event {
            DriverEvent::Client(client_event) => dispatch_client_event(&inner, *client_event),
            DriverEvent::ResumeState(state) => {
                inner.borrow_mut().resume_state = state;
            }
            DriverEvent::Error(description) => emit_error_callback(&inner, &description),
            DriverEvent::Disconnected => {
                let command_owner = inner.borrow().command_owner.upgrade();
                if let Some(command_owner) = command_owner {
                    command_owner.borrow_mut().take();
                }
                // Clone the callback before invoking it so a JS handler that
                // synchronously re-enters the WaddleClient via a `send_*`
                // method (which takes `borrow_mut`) does not panic on an
                // outstanding `Rc<RefCell>` borrow held across the call. The
                // same pattern is applied to every other `on_*` dispatch site
                // below.
                let callback = inner.borrow().on_disconnected.clone();
                if let Some(callback) = callback {
                    let _ = callback.call0(&JsValue::NULL);
                }
            }
        }
    }
}

pub(crate) fn dispatch_client_event(inner: &Rc<RefCell<WaddleClientInner>>, event: ClientEvent) {
    match event {
        ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_)) => {
            // Snapshot both callbacks BEFORE invoking either; a JS handler
            // that re-enters `WaddleClient::set_on_*` would otherwise drop
            // the live borrow mid-iteration.
            let (on_connected, on_session_lifecycle) = {
                let borrowed = inner.borrow();
                (
                    borrowed.on_connected.clone(),
                    borrowed.on_session_lifecycle.clone(),
                )
            };
            if let Some(callback) = on_connected {
                let _ = callback.call0(&JsValue::NULL);
            }
            if let Some(callback) = on_session_lifecycle {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str("fresh"));
            }
        }
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Resumed { .. },
        )) => {
            let callback = inner.borrow().on_session_lifecycle.clone();
            if let Some(callback) = callback {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str("resumed"));
            }
        }
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::AckRequestSent { attempt, unacked },
        )) => emit_stream_management_callback(
            inner,
            JsStreamManagementTelemetry::Request { attempt, unacked },
        ),
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::AckObserved {
                progressed,
                latency_ms,
                unacked,
            },
        )) => emit_stream_management_callback(
            inner,
            JsStreamManagementTelemetry::Observed {
                progressed,
                latency_ms,
                unacked,
            },
        ),
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::AckRequestTimedOut { unacked },
        )) => emit_stream_management_callback(
            inner,
            JsStreamManagementTelemetry::RequestTimeout { unacked },
        ),
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::AckProgressStalled {
                unacked,
                elapsed_ms,
            },
        )) => emit_stream_management_callback(
            inner,
            JsStreamManagementTelemetry::ProgressStalled {
                unacked,
                elapsed_ms,
            },
        ),
        ClientEvent::Connection(ConnectionEvent::StreamError { condition, detail }) => {
            emit_stream_error_callback(inner, condition, detail);
        }
        ClientEvent::Messaging(waddle_xmpp_client::MessagingEvent::Message(message)) => {
            let mut message = *message;
            let account_bare_jid = {
                let borrowed = inner.borrow();
                bare_jid(&borrowed.config.jid)
            };
            if message.mds_displayed.is_some()
                && !waddle_xmpp_client::mds::mds_event_from_matches_account(
                    message.from.as_deref(),
                    &account_bare_jid,
                )
            {
                message.mds_displayed = None;
            }
            // XEP-0490 §3.2: surface MDS PEP events through the
            // dedicated `on_mds_displayed` callback as well as the
            // generic on_message stream. Either path is safe; the
            // chat layer applies state from `on_mds_displayed` and
            // drops the message-shaped echo for MDS events.
            if let Some(entries) = message.mds_displayed.as_ref() {
                let callback = inner.borrow().on_mds_displayed.clone();
                if let Some(callback) = callback {
                    for entry in entries {
                        let js_entry = WaddleMdsDisplayedEntry {
                            chat_id: entry.chat_id.to_string(),
                            stanza_id: entry.stanza_id.as_str().to_string(),
                            stanza_id_by: entry.stanza_id_by.to_string(),
                        };
                        if let Ok(value) = to_js_value(&js_entry) {
                            let _ = callback.call1(&JsValue::NULL, &value);
                        }
                    }
                }
            }
            if !message.pubsub_events.is_empty() {
                let callback = inner.borrow().on_pubsub_event.clone();
                if let Some(callback) = callback {
                    for event in message.pubsub_events.iter().cloned() {
                        if let Ok(value) = to_js_value(&pubsub_event_to_js(event)) {
                            let _ = callback.call1(&JsValue::NULL, &value);
                        }
                    }
                }
            }
            let callback = inner.borrow().on_message.clone();
            if let Some(callback) = callback {
                if let Ok(value) = to_js_value(&inbound_to_js(message)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        ClientEvent::Messaging(waddle_xmpp_client::MessagingEvent::Presence(presence)) => {
            let callback = inner.borrow().on_presence.clone();
            if let Some(callback) = callback {
                if let Ok(value) = to_js_value(&presence_to_js(*presence)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        ClientEvent::InboxStreamEntry(entry) => {
            let callback = inner.borrow().on_message.clone();
            if let Some(callback) = callback {
                if let Ok(value) = to_js_value(&inbox_push_to_js(entry)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id }) => {
            let callback = inner.borrow().on_message_delivery_acked.clone();
            if let Some(callback) = callback {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(stanza_id.as_str()));
            }
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id }) => {
            let callback = inner.borrow().on_message_delivery_failed.clone();
            if let Some(callback) = callback {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(stanza_id.as_str()));
            }
        }
        ClientEvent::Call(call_event) => {
            let callback = inner.borrow().on_call.clone();
            if let Some(callback) = callback {
                if let Ok(value) = to_js_value(&call_event_to_js(*call_event)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        _ => {}
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
enum JsStreamManagementTelemetry {
    #[serde(rename = "ack-request")]
    Request { attempt: u32, unacked: u32 },
    #[serde(rename = "ack-observed")]
    Observed {
        progressed: bool,
        latency_ms: Option<u64>,
        unacked: u32,
    },
    #[serde(rename = "ack-request-timeout")]
    RequestTimeout { unacked: u32 },
    #[serde(rename = "ack-progress-stalled")]
    ProgressStalled { unacked: u32, elapsed_ms: u64 },
}

fn emit_stream_management_callback(
    inner: &Rc<RefCell<WaddleClientInner>>,
    event: JsStreamManagementTelemetry,
) {
    let callback = inner.borrow().on_stream_management.clone();
    if let Some(callback) = callback {
        if let Ok(value) = to_js_value(&event) {
            let _ = callback.call1(&JsValue::NULL, &value);
        }
    }
}

pub(crate) fn emit_error_callback(inner: &Rc<RefCell<WaddleClientInner>>, description: &str) {
    let callback = inner.borrow().on_error.clone();
    if let Some(callback) = callback {
        let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(description));
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsStreamError<'a> {
    detail: &'a str,
    condition: &'a str,
    stream_management_error: Option<JsStreamManagementError>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
enum JsStreamManagementError {
    #[serde(rename = "handled-count-too-high")]
    HandledCountTooHigh { h: u32, send_count: u32 },
}

fn emit_stream_error_callback(
    inner: &Rc<RefCell<WaddleClientInner>>,
    condition: StreamErrorCondition,
    detail: Option<StreamErrorDetail>,
) {
    let callback = inner.borrow().on_error.clone();
    if let Some(callback) = callback {
        let condition = condition.as_str();
        let stream_management_error = stream_management_error_to_js(detail);
        let detail = stream_error_detail_label(condition, stream_management_error.as_ref());
        let payload = JsStreamError {
            detail,
            condition,
            stream_management_error,
        };
        let value = to_js_value(&payload).unwrap_or_else(|_| JsValue::from_str(condition));
        let _ = callback.call1(&JsValue::NULL, &value);
    }
}

fn stream_management_error_to_js(
    detail: Option<StreamErrorDetail>,
) -> Option<JsStreamManagementError> {
    detail.map(|StreamErrorDetail::HandledCountTooHigh { h, send_count }| {
        JsStreamManagementError::HandledCountTooHigh { h, send_count }
    })
}

fn stream_error_detail_label(
    condition: &'static str,
    stream_management_error: Option<&JsStreamManagementError>,
) -> &'static str {
    match stream_management_error {
        Some(JsStreamManagementError::HandledCountTooHigh { .. }) => "handled-count-too-high",
        None => condition,
    }
}
