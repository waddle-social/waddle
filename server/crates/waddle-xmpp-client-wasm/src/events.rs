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
                if let Some(callback) = inner.borrow().on_disconnected.as_ref() {
                    let _ = callback.call0(&JsValue::NULL);
                }
            }
        }
    }
}

pub(crate) fn dispatch_client_event(inner: &Rc<RefCell<WaddleClientInner>>, event: ClientEvent) {
    match event {
        ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_)) => {
            let borrowed = inner.borrow();
            if let Some(callback) = borrowed.on_connected.as_ref() {
                let _ = callback.call0(&JsValue::NULL);
            }
            if let Some(callback) = borrowed.on_session_lifecycle.as_ref() {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str("fresh"));
            }
        }
        ClientEvent::Connection(ConnectionEvent::StreamManagement(
            StreamManagementEvent::Resumed { .. },
        )) => {
            if let Some(callback) = inner.borrow().on_session_lifecycle.as_ref() {
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
            if let Some(callback) = inner.borrow().on_message.as_ref() {
                if let Ok(value) = to_js_value(&inbound_to_js(*message)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        ClientEvent::Messaging(waddle_xmpp_client::MessagingEvent::Presence(presence)) => {
            if let Some(callback) = inner.borrow().on_presence.as_ref() {
                if let Ok(value) = to_js_value(&presence_to_js(presence)) {
                    let _ = callback.call1(&JsValue::NULL, &value);
                }
            }
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id }) => {
            if let Some(callback) = inner.borrow().on_message_delivery_acked.as_ref() {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(stanza_id.as_str()));
            }
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id }) => {
            if let Some(callback) = inner.borrow().on_message_delivery_failed.as_ref() {
                let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(stanza_id.as_str()));
            }
        }
        ClientEvent::UnhandledStanza(element) => {
            // Try to recognise an A/V call event (XEP-0353 JMI
            // envelope or XEP-0166 Jingle session control with a
            // urn:waddle:transports:livekit:0 transport). If matched,
            // surface as a typed `on_call` callback so the chat side
            // doesn't have to parse XML.
            if let Some(call_event) = parse_call_event(&element) {
                if let Some(callback) = inner.borrow().on_call.as_ref() {
                    if let Ok(value) = to_js_value(&call_event_to_js(call_event)) {
                        let _ = callback.call1(&JsValue::NULL, &value);
                    }
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn emit_error_callback(inner: &Rc<RefCell<WaddleClientInner>>, description: &str) {
    if let Some(callback) = inner.borrow().on_error.as_ref() {
        let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(description));
    }
}
