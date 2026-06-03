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
                inner.borrow_mut().cmd_tx = None;
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
        ClientEvent::Messaging(waddle_xmpp_client::MessagingEvent::Message(message)) => {
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
                            chat_id: entry.chat_id.clone(),
                            stanza_id: entry.stanza_id.clone(),
                            stanza_id_by: entry.stanza_id_by.clone(),
                        };
                        if let Ok(value) = to_js_value(&js_entry) {
                            let _ = callback.call1(&JsValue::NULL, &value);
                        }
                    }
                }
            }
            let callback = inner.borrow().on_message.clone();
            if let Some(callback) = callback {
                if let Ok(value) = to_js_value(&inbound_to_js(*message)) {
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

pub(crate) fn emit_error_callback(inner: &Rc<RefCell<WaddleClientInner>>, description: &str) {
    let callback = inner.borrow().on_error.clone();
    if let Some(callback) = callback {
        let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(description));
    }
}
